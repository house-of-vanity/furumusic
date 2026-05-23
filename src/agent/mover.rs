use std::path::{Path, PathBuf};

pub enum MoveOutcome {
    /// File was moved/renamed to destination.
    Moved(PathBuf),
    /// Destination already existed; inbox duplicate was removed.
    Merged(PathBuf),
}

/// Move a file from inbox to the permanent storage directory.
///
/// Creates the directory structure: `storage_dir/artist/album/filename`
///
/// If `rename` fails (cross-device), falls back to copy + remove.
/// If the destination already exists the inbox copy is removed and
/// `MoveOutcome::Merged` is returned.
pub async fn move_to_storage(
    storage_dir: &Path,
    artist: &str,
    album: &str,
    filename: &str,
    source: &Path,
) -> anyhow::Result<MoveOutcome> {
    let artist_dir = sanitize_dir_name(artist);
    let album_dir = sanitize_dir_name(album);

    let dest_dir = storage_dir.join(&artist_dir).join(&album_dir);
    tokio::fs::create_dir_all(&dest_dir).await?;

    let dest = dest_dir.join(filename);

    // File already at destination — remove the inbox duplicate
    if dest.exists() {
        if source.exists() {
            tokio::fs::remove_file(source).await?;
            tracing::info!(from = ?source, to = ?dest, "merged duplicate into existing storage file");
        }
        return Ok(MoveOutcome::Merged(dest));
    }

    // Try atomic rename first (same filesystem)
    match tokio::fs::rename(source, &dest).await {
        Ok(()) => {}
        Err(_) => {
            // Cross-device: copy then remove
            tokio::fs::copy(source, &dest).await?;
            tokio::fs::remove_file(source).await?;
        }
    }

    tracing::info!(from = ?source, to = ?dest, "moved file to storage");
    Ok(MoveOutcome::Moved(dest))
}

/// Remove characters that are unsafe for directory names.
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
