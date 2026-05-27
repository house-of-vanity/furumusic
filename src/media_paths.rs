use std::path::{Component, Path, PathBuf};

const KNOWN_MEDIA_ROOTS: &[&str] = &["media/library", "media/uploads"];

pub fn resolve_config_path(value: &str) -> String {
    let path = resolve_config_path_buf(value);
    if path.as_os_str().is_empty() {
        String::new()
    } else {
        path.to_string_lossy().to_string()
    }
}

pub fn resolve_config_path_buf(value: &str) -> PathBuf {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return PathBuf::new();
    }

    let normalized = normalize_slashes(trimmed);
    if is_host_absolute(&normalized) {
        return PathBuf::from(normalized);
    }

    if looks_like_windows_absolute(&normalized) {
        if let Some(relative) = extract_known_media_root(&normalized) {
            return app_root().join(slash_path(&relative));
        }
        return PathBuf::from(normalized);
    }

    app_root().join(slash_path(&normalized))
}

pub fn resolve_media_file_path(storage_dir: &str, file_path: &str) -> PathBuf {
    let storage_root = resolve_config_path_buf(storage_dir);
    if let Some(relative) = normalize_stored_media_file_path(storage_dir, file_path) {
        return storage_root.join(slash_path(&relative));
    }

    let normalized = normalize_slashes(file_path.trim());
    let path = PathBuf::from(&normalized);
    if path.is_absolute() {
        path
    } else {
        storage_root.join(path)
    }
}

pub fn media_file_path_for_storage(storage_dir: &str, path: &Path) -> Option<String> {
    let storage_root = resolve_config_path_buf(storage_dir);
    if let Ok(relative) = path.strip_prefix(&storage_root) {
        return relative_path_string(relative);
    }

    let normalized = normalize_slashes(&path.to_string_lossy());
    relative_after_storage_marker(&storage_root, &normalized).or_else(|| {
        if !is_host_absolute(&normalized) && !looks_like_windows_absolute(&normalized) {
            normalize_relative_path(&normalized)
        } else {
            None
        }
    })
}

pub fn normalize_stored_media_file_path(storage_dir: &str, file_path: &str) -> Option<String> {
    let trimmed = file_path.trim();
    if trimmed.is_empty() {
        return None;
    }

    let storage_root = resolve_config_path_buf(storage_dir);
    let normalized = normalize_slashes(trimmed);
    let path = PathBuf::from(&normalized);
    if path.is_absolute() {
        if let Ok(relative) = path.strip_prefix(&storage_root) {
            return relative_path_string(relative);
        }
        return relative_after_storage_marker(&storage_root, &normalized);
    }

    if looks_like_windows_absolute(&normalized) {
        return relative_after_storage_marker(&storage_root, &normalized);
    }

    if let Some(relative) = relative_after_storage_marker_prefix(&storage_root, &normalized) {
        return Some(relative);
    }

    normalize_relative_path(&normalized)
}

pub async fn normalize_media_file_paths(
    pool: &sqlx::PgPool,
    storage_dir: &str,
) -> anyhow::Result<u64> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, file_path FROM furumusic__media_file ORDER BY id")
            .fetch_all(pool)
            .await?;

    let mut updated = 0;
    for (id, file_path) in rows {
        let Some(relative) = normalize_stored_media_file_path(storage_dir, &file_path) else {
            continue;
        };
        if relative == file_path {
            continue;
        }
        sqlx::query("UPDATE furumusic__media_file SET file_path = $1 WHERE id = $2")
            .bind(&relative)
            .bind(id)
            .execute(pool)
            .await?;
        updated += 1;
    }

    Ok(updated)
}

fn app_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn normalize_slashes(value: &str) -> String {
    value.trim().replace('\\', "/")
}

fn is_host_absolute(value: &str) -> bool {
    Path::new(value).is_absolute()
}

fn looks_like_windows_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic()
}

fn slash_path(value: &str) -> PathBuf {
    value
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .fold(PathBuf::new(), |mut path, part| {
            path.push(part);
            path
        })
}

fn normalize_relative_path(value: &str) -> Option<String> {
    let parts: Vec<&str> = value
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() || parts.iter().any(|part| *part == "..") {
        return None;
    }
    Some(parts.join("/"))
}

fn relative_path_string(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn extract_known_media_root(value: &str) -> Option<String> {
    KNOWN_MEDIA_ROOTS
        .iter()
        .filter_map(|marker| relative_from_marker(value, marker, true))
        .next()
}

fn relative_after_storage_marker(storage_root: &Path, value: &str) -> Option<String> {
    let marker = storage_marker(storage_root)?;
    relative_from_marker(value, &marker, false)
}

fn relative_after_storage_marker_prefix(storage_root: &Path, value: &str) -> Option<String> {
    let marker = storage_marker(storage_root)?;
    let normalized = normalize_slashes(value);
    let normalized_lower = normalized.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    if normalized_lower == marker_lower {
        return None;
    }
    normalized_lower
        .strip_prefix(&(marker_lower + "/"))
        .and_then(|_| normalize_relative_path(&normalized[marker.len() + 1..]))
}

fn storage_marker(storage_root: &Path) -> Option<String> {
    let parts: Vec<String> = storage_root
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    if parts.len() >= 2 {
        Some(format!(
            "{}/{}",
            parts[parts.len() - 2],
            parts[parts.len() - 1]
        ))
    } else {
        parts.last().cloned()
    }
}

fn relative_from_marker(value: &str, marker: &str, include_marker: bool) -> Option<String> {
    let normalized = normalize_slashes(value);
    let haystack = format!("/{}", normalized.trim_matches('/'));
    let marker = marker.trim_matches('/');
    let needle = format!("/{marker}");
    let haystack_lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let index = haystack_lower.rfind(&needle_lower)?;
    let after_marker = index + needle.len();
    if after_marker < haystack.len() && haystack.as_bytes().get(after_marker) != Some(&b'/') {
        return None;
    }
    let tail = haystack[after_marker..].trim_matches('/');
    if include_marker {
        if tail.is_empty() {
            Some(marker.to_string())
        } else {
            Some(format!("{marker}/{tail}"))
        }
    } else {
        normalize_relative_path(tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_config_path_from_app_root() {
        let expected = app_root().join("media").join("library");
        assert_eq!(resolve_config_path_buf("media/library"), expected);
    }

    #[test]
    fn maps_foreign_windows_config_media_root_to_app_root() {
        let expected = app_root().join("media").join("uploads");
        assert_eq!(
            resolve_config_path_buf(r"C:\Users\ab\repos\furumusic\media\uploads"),
            expected
        );
    }

    #[test]
    fn stores_path_relative_to_storage_root() {
        let storage = app_root().join("media").join("library");
        let path = storage.join("Artist").join("Album").join("track.flac");
        assert_eq!(
            media_file_path_for_storage(&storage.to_string_lossy(), &path).as_deref(),
            Some("Artist/Album/track.flac")
        );
    }

    #[test]
    fn normalizes_legacy_windows_media_file_path() {
        let storage = app_root().join("media").join("library");
        assert_eq!(
            normalize_stored_media_file_path(
                &storage.to_string_lossy(),
                r"C:\Users\ab\repos\furumusic\media\library\Buckethead\Pike\cover.jpg",
            )
            .as_deref(),
            Some("Buckethead/Pike/cover.jpg")
        );
    }

    #[test]
    fn strips_accidental_relative_storage_root_prefix() {
        let storage = app_root().join("media").join("library");
        assert_eq!(
            normalize_stored_media_file_path(
                &storage.to_string_lossy(),
                "media/library/Buckethead/Pike/cover.jpg",
            )
            .as_deref(),
            Some("Buckethead/Pike/cover.jpg")
        );
    }

    #[test]
    fn resolves_legacy_windows_media_file_path_to_current_storage() {
        let storage = app_root().join("media").join("library");
        assert_eq!(
            resolve_media_file_path(
                &storage.to_string_lossy(),
                r"C:\Users\ab\repos\furumusic\media\library\Buckethead\Pike\cover.jpg",
            ),
            storage.join("Buckethead").join("Pike").join("cover.jpg")
        );
    }
}
