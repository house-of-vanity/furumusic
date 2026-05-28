use std::path::{Component, Path, PathBuf};

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
    if is_absolute_path(&normalized) {
        PathBuf::from(normalized)
    } else {
        app_root().join(slash_path(&normalized))
    }
}

pub fn resolve_media_file_path(storage_dir: &str, file_path: &str) -> PathBuf {
    resolve_path_from_root(storage_dir, file_path)
}

pub fn media_file_path_for_storage(storage_dir: &str, path: &Path) -> Option<String> {
    path_for_root(storage_dir, path)
}

pub fn resolve_path_from_root(root_dir: &str, stored_path: &str) -> PathBuf {
    let normalized = normalize_slashes(stored_path.trim());
    if is_absolute_path(&normalized) {
        PathBuf::from(normalized)
    } else {
        resolve_config_path_buf(root_dir).join(slash_path(&normalized))
    }
}

pub fn path_for_root(root_dir: &str, path: &Path) -> Option<String> {
    let root = resolve_config_path_buf(root_dir);
    let normalized = normalize_slashes(&path.to_string_lossy());
    if is_absolute_path(&normalized) {
        return strip_root_prefix(&root, &normalized);
    }

    relative_path_string(path)
}

pub async fn normalize_media_file_paths(
    pool: &sqlx::PgPool,
    storage_dir: &str,
) -> anyhow::Result<u64> {
    normalize_table_paths(pool, "furumusic__media_file", "file_path", storage_dir).await
}

pub async fn normalize_pending_review_paths(
    pool: &sqlx::PgPool,
    inbox_dir: &str,
) -> anyhow::Result<u64> {
    normalize_table_paths(pool, "furumusic__pending_review", "input_path", inbox_dir).await
}

async fn normalize_table_paths(
    pool: &sqlx::PgPool,
    table: &str,
    column: &str,
    root_dir: &str,
) -> anyhow::Result<u64> {
    let sql = format!("SELECT id, {column} FROM {table} WHERE {column} IS NOT NULL ORDER BY id");
    let rows: Vec<(i64, String)> = sqlx::query_as(&sql).fetch_all(pool).await?;

    let mut updated = 0;
    for (id, stored_path) in rows {
        let Some(normalized) = normalize_stored_path(root_dir, &stored_path) else {
            continue;
        };
        if normalized == stored_path {
            continue;
        }

        let sql = format!("UPDATE {table} SET {column} = $1 WHERE id = $2");
        sqlx::query(&sql)
            .bind(&normalized)
            .bind(id)
            .execute(pool)
            .await?;
        updated += 1;
    }

    Ok(updated)
}

fn normalize_stored_path(root_dir: &str, stored_path: &str) -> Option<String> {
    let normalized = normalize_slashes(stored_path);
    if normalized.is_empty() {
        return None;
    }

    if is_absolute_path(&normalized) {
        strip_root_prefix(&resolve_config_path_buf(root_dir), &normalized)
    } else {
        normalize_relative_path(&normalized)
    }
}

fn app_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn normalize_slashes(value: &str) -> String {
    value.trim().replace('\\', "/")
}

fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/') || Path::new(value).is_absolute() || looks_like_windows_absolute(value)
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

fn strip_root_prefix(root: &Path, normalized_path: &str) -> Option<String> {
    let root_string = normalize_slashes(&root.to_string_lossy());
    let root_trimmed = root_string.trim_end_matches('/');
    let path_trimmed = normalized_path.trim();

    let root_cmp = comparable_path(root_trimmed);
    let path_cmp = comparable_path(path_trimmed);
    if path_cmp == root_cmp {
        return None;
    }

    let prefix = format!("{root_cmp}/");
    if path_cmp.starts_with(&prefix) {
        let tail = &path_trimmed[root_trimmed.len() + 1..];
        return normalize_relative_path(tail);
    }

    None
}

fn comparable_path(value: &str) -> String {
    let normalized = normalize_slashes(value).trim_end_matches('/').to_owned();
    if cfg!(windows) || looks_like_windows_absolute(&normalized) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_config_path_from_app_root() {
        let expected = app_root().join("media").join("library");
        assert_eq!(resolve_config_path_buf("media/library"), expected);
    }

    #[test]
    fn keeps_absolute_config_path() {
        assert_eq!(resolve_config_path_buf("/media"), PathBuf::from("/media"));
    }

    #[test]
    fn resolves_relative_media_file_under_storage_root() {
        assert_eq!(
            resolve_media_file_path("/media", "Buckethead/Pike/cover.jpg"),
            PathBuf::from("/media")
                .join("Buckethead")
                .join("Pike")
                .join("cover.jpg")
        );
    }

    #[test]
    fn keeps_absolute_media_file_path() {
        assert_eq!(
            resolve_media_file_path("/media", "/media/Buckethead/Pike/cover.jpg"),
            PathBuf::from("/media/Buckethead/Pike/cover.jpg")
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
    fn stores_windows_path_relative_to_windows_storage_root() {
        assert_eq!(
            path_for_root(
                r"C:\Users\ab\repos\furumusic\library",
                Path::new(r"C:\Users\ab\repos\furumusic\library\Artist\Album\track.mp3"),
            )
            .as_deref(),
            Some("Artist/Album/track.mp3")
        );
    }

    #[test]
    fn normalizes_relative_backslashes() {
        assert_eq!(
            normalize_stored_path("/media", r"Artist\Album\track.mp3").as_deref(),
            Some("Artist/Album/track.mp3")
        );
    }
}
