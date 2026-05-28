//! Cover art extraction and management.
//!
//! Sources (in priority order):
//! 1. Standalone image files in the album folder (cover.jpg, folder.jpg, etc.)
//! 2. Embedded cover art in audio file metadata (ID3 APIC, Vorbis METADATA_BLOCK_PICTURE, etc.)
//! 3. Remote metadata providers used by background backfill jobs.
//!
//! The first usable image found is saved as a MediaFile with file_type="cover_art"
//! and linked to the Release via cover_file_id.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Image data extracted from an audio file or found on disk.
#[derive(Debug)]
pub struct CoverImage {
    pub data: Vec<u8>,
    pub mime_type: String,
    /// Where this image came from (for logging).
    pub source: CoverSource,
}

#[derive(Debug)]
pub enum CoverSource {
    /// A standalone image file in the folder.
    FolderFile(PathBuf),
    /// Embedded in an audio file's metadata.
    Embedded(PathBuf),
    /// Downloaded from a remote metadata provider.
    Remote(String),
}

/// Well-known cover art filenames, in priority order.
/// Case-insensitive matching is used.
const COVER_FILENAMES: &[&str] = &[
    "cover",
    "folder",
    "front",
    "album",
    "albumart",
    "albumartsmall",
    "thumb",
    "artwork",
];

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];

fn is_image_file(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    IMAGE_EXTENSIONS.contains(&ext.as_str())
}

fn mime_for_image(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "png" => "image/png".to_string(),
        "webp" => "image/webp".to_string(),
        "gif" => "image/gif".to_string(),
        "bmp" => "image/bmp".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Scan a folder for image files that look like cover art.
///
/// Returns image file paths sorted by priority:
/// - Files with well-known names (cover.jpg, front.png, etc.) first
/// - Then any other image files
pub fn find_folder_images(folder: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(folder) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut images: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            !name.starts_with('.') && is_image_file(&name)
        })
        .map(|e| e.path())
        .collect();

    // Sort: well-known names first (by priority index), then alphabetically
    images.sort_by(|a, b| {
        let pri_a = cover_name_priority(a);
        let pri_b = cover_name_priority(b);
        pri_a.cmp(&pri_b).then_with(|| a.cmp(b))
    });

    images
}

/// Return a priority index for a filename (lower = higher priority).
/// Well-known cover filenames get indices 0..N, unknown ones get usize::MAX.
fn cover_name_priority(path: &Path) -> usize {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    for (i, &known) in COVER_FILENAMES.iter().enumerate() {
        if stem == known {
            return i;
        }
    }
    usize::MAX
}

/// Try to find the best cover image for a folder of audio files.
///
/// Strategy:
/// 1. Look for standalone image files in the folder (prioritized by filename).
/// 2. Try to extract embedded cover art from each audio file.
///
/// Returns the first usable image found, or None.
pub async fn find_best_cover(folder: &Path, audio_files: &[PathBuf]) -> Option<CoverImage> {
    // Strategy 1: folder images
    let folder_images = find_folder_images(folder);
    for img_path in &folder_images {
        match tokio::fs::read(img_path).await {
            Ok(data) if !data.is_empty() => {
                let mime = mime_for_image(img_path);
                return Some(CoverImage {
                    data,
                    mime_type: mime,
                    source: CoverSource::FolderFile(img_path.clone()),
                });
            }
            _ => continue,
        }
    }

    // Strategy 2: embedded cover art from audio files
    for audio_path in audio_files {
        let path = audio_path.to_path_buf();
        let result = tokio::task::spawn_blocking(move || extract_embedded_cover(&path)).await;
        if let Ok(Some(cover)) = result {
            return Some(cover);
        }
    }

    None
}

/// Extract embedded cover art from an audio file.
///
/// Tries Symphonia first (works for FLAC, OGG, etc.), then falls back to
/// id3 crate for MP3 files.
///
/// Must be called from a blocking context.
fn extract_embedded_cover(path: &Path) -> Option<CoverImage> {
    // Try Symphonia visuals first
    if let Some(cover) = extract_cover_symphonia(path) {
        return Some(cover);
    }

    // Fallback: id3 for MP3
    let is_mp3 = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false);

    if is_mp3 {
        return extract_cover_id3(path);
    }

    None
}

fn extract_cover_symphonia(path: &Path) -> Option<CoverImage> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: false,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .ok()?;

    // Check side-data metadata (ID3 before format)
    if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        for visual in rev.visuals() {
            if !visual.data.is_empty() {
                let mime = if visual.media_type.is_empty() {
                    guess_image_mime(&visual.data)
                } else {
                    visual.media_type.to_string()
                };
                return Some(CoverImage {
                    data: visual.data.to_vec(),
                    mime_type: mime,
                    source: CoverSource::Embedded(path.to_path_buf()),
                });
            }
        }
    }

    // Check format-level metadata
    if let Some(rev) = probed.format.metadata().current() {
        for visual in rev.visuals() {
            if !visual.data.is_empty() {
                let mime = if visual.media_type.is_empty() {
                    guess_image_mime(&visual.data)
                } else {
                    visual.media_type.to_string()
                };
                return Some(CoverImage {
                    data: visual.data.to_vec(),
                    mime_type: mime,
                    source: CoverSource::Embedded(path.to_path_buf()),
                });
            }
        }
    }

    None
}

fn extract_cover_id3(path: &Path) -> Option<CoverImage> {
    let tag = id3::Tag::read_from_path(path).ok()?;

    // Prefer front cover (picture type 3), then any picture
    let mut best: Option<&id3::frame::Picture> = None;
    for pic in tag.pictures() {
        if pic.picture_type == id3::frame::PictureType::CoverFront {
            best = Some(pic);
            break;
        }
        if best.is_none() {
            best = Some(pic);
        }
    }

    let pic = best?;
    if pic.data.is_empty() {
        return None;
    }

    let mime = if pic.mime_type.is_empty() || pic.mime_type == "image/" {
        guess_image_mime(&pic.data)
    } else {
        pic.mime_type.clone()
    };

    Some(CoverImage {
        data: pic.data.clone(),
        mime_type: mime,
        source: CoverSource::Embedded(path.to_path_buf()),
    })
}

/// Guess MIME type from image magic bytes.
fn guess_image_mime(data: &[u8]) -> String {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg".to_string()
    } else if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        "image/png".to_string()
    } else if data.starts_with(b"RIFF") && data.len() > 12 && &data[8..12] == b"WEBP" {
        "image/webp".to_string()
    } else if data.starts_with(b"GIF8") {
        "image/gif".to_string()
    } else if data.starts_with(&[0x42, 0x4D]) {
        "image/bmp".to_string()
    } else {
        "image/jpeg".to_string() // default assumption
    }
}

/// Compute SHA-256 hash of image data.
pub fn hash_image(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    format!("{:x}", digest)
}

/// Extension for a MIME type.
pub fn extension_for_mime(mime: &str) -> &str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "jpg",
    }
}

/// Save cover image data to the storage directory and create a MediaFile record.
///
/// Returns the MediaFile ID on success.
pub async fn save_cover_to_storage(
    db: &cot::db::Database,
    pool: &sqlx::PgPool,
    storage_dir: &str,
    artist_name: &str,
    release_title: &str,
    cover: &CoverImage,
) -> anyhow::Result<i64> {
    let hash = hash_image(&cover.data);

    // Check if we already have this exact image in the DB
    let existing: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, file_path FROM furumusic__media_file WHERE sha256_hash = $1 AND file_type = 'cover_art' LIMIT 1",
    )
    .bind(&hash)
    .fetch_optional(pool)
    .await?;

    if let Some((id, file_path)) = existing {
        let path = crate::media_paths::resolve_media_file_path(storage_dir, &file_path);
        let is_inside_storage = crate::media_paths::path_for_root(storage_dir, &path).is_some();
        if !is_inside_storage {
            tracing::warn!(
                media_file_id = id,
                path = %path.display(),
                "Ignoring duplicate cover hash whose stored file is outside agent_storage_dir"
            );
        } else if !path.exists() {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            match tokio::fs::write(&path, &cover.data).await {
                Ok(()) => {
                    tracing::info!(
                        media_file_id = id,
                        path = %path.display(),
                        "Restored missing cover file for existing MediaFile"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        media_file_id = id,
                        path = %path.display(),
                        error = %err,
                        "Failed to restore missing cover file for existing MediaFile; creating a new cover file"
                    );
                }
            }
        }
        if is_inside_storage && path.exists() {
            if let Err(err) = crate::agent::cover_variants::ensure_cover_variants(&path).await {
                tracing::warn!(media_file_id = id, error = %err, "Failed to generate cover variants");
            }
            return Ok(id);
        }
    }

    let ext = extension_for_mime(&cover.mime_type);
    let filename = format!("cover.{ext}");

    let artist_dir = sanitize_dir_name(artist_name);
    let album_dir = sanitize_dir_name(release_title);

    let dest_dir = crate::media_paths::resolve_config_path_buf(storage_dir)
        .join(&artist_dir)
        .join(&album_dir);
    tokio::fs::create_dir_all(&dest_dir).await?;

    let dest_path = dest_dir.join(&filename);

    // Write image data
    tokio::fs::write(&dest_path, &cover.data).await?;

    let relative_path = crate::media_paths::media_file_path_for_storage(storage_dir, &dest_path)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cover destination is outside agent_storage_dir: {}",
                dest_path.display()
            )
        })?;
    let file_size = cover.data.len() as i64;

    let media_file = crate::music::MediaFile::create(
        db,
        "cover_art",
        &relative_path,
        &filename,
        &cover.mime_type,
        file_size,
        &hash,
        None,
        None,
        None,
        None,
        None,
        Some("UFO"),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to create cover MediaFile: {e}"))?;

    tracing::info!(
        media_file_id = media_file.id_val(),
        hash = %hash,
        mime = %cover.mime_type,
        size = file_size,
        "Saved cover art"
    );

    if let Err(err) = crate::agent::cover_variants::ensure_cover_variants(&dest_path).await {
        tracing::warn!(
            media_file_id = media_file.id_val(),
            error = %err,
            "Failed to generate cover variants"
        );
    }

    Ok(media_file.id_val())
}

/// Set the cover_file_id on a release (if not already set).
pub async fn assign_cover_to_release(
    pool: &sqlx::PgPool,
    release_id: i64,
    cover_file_id: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE furumusic__release SET cover_file_id = $1, updated_at = $3 WHERE id = $2 AND cover_file_id IS NULL",
    )
    .bind(cover_file_id)
    .bind(release_id)
    .bind(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .execute(pool)
    .await?;
    Ok(())
}

fn sanitize_dir_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_owned()
}
