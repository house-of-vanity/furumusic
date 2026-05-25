use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::scheduler::{Job, JobContext, JobLog, PendingReview};

/// Guard to prevent overlapping inbox_discover runs.
static DISCOVER_RUNNING: AtomicBool = AtomicBool::new(false);

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "aac", "m4a", "wav", "ape", "wv", "wma", "tta", "aiff", "aif",
];

pub struct InboxDiscoverJob;

#[async_trait::async_trait]
impl Job for InboxDiscoverJob {
    fn name(&self) -> &'static str {
        "inbox_discover"
    }

    fn description(&self) -> &'static str {
        "Scan inbox for new audio files and queue them for processing"
    }

    fn default_cron(&self) -> &'static str {
        "0 */5 * * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        // Prevent overlapping discover runs
        if DISCOVER_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            log.info("Another inbox_discover is already running, skipping");
            return Ok(());
        }
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                DISCOVER_RUNNING.store(false, Ordering::SeqCst);
            }
        }
        let _guard = Guard;

        let config = &ctx.config;

        if config.agent_inbox_dir.is_empty() {
            log.info("No inbox directory configured, skipping");
            return Ok(());
        }

        let inbox = Path::new(&config.agent_inbox_dir);
        if !inbox.exists() {
            log.warn(&format!("Inbox path does not exist: {}", inbox.display()));
            return Ok(());
        }

        let mut audio_files = Vec::new();
        collect_audio_files(inbox, &mut audio_files).await?;

        log.info(&format!("Found {} audio files in inbox", audio_files.len()));
        if audio_files.is_empty() {
            return Ok(());
        }

        let groups = group_by_folder(&audio_files);
        log.info(&format!("Grouped into {} folder batches", groups.len()));

        let mut discovered = 0u64;
        let mut skipped_hash = 0u64;
        let mut skipped_existing = 0u64;

        for (_folder, files) in &groups {
            for file_path in files {
                let input_path_str = file_path.to_string_lossy().to_string();

                // Skip if a PendingReview already exists for this path
                match PendingReview::exists_for_path(&ctx.db, &input_path_str).await {
                    Ok(true) => {
                        skipped_existing += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        log.warn(&format!(
                            "Error checking existing review for {}: {e}",
                            input_path_str
                        ));
                        continue;
                    }
                }

                // Compute SHA-256 hash
                let path_clone = file_path.to_path_buf();
                let (hash, file_size) =
                    match tokio::task::spawn_blocking(move || -> anyhow::Result<(String, i64)> {
                        let data = std::fs::read(&path_clone)?;
                        let digest = Sha256::digest(&data);
                        let hash = format!("{:x}", digest);
                        let size = data.len() as i64;
                        Ok((hash, size))
                    })
                    .await?
                    {
                        Ok(v) => v,
                        Err(e) => {
                            log.warn(&format!("Failed to hash {}: {e}", file_path.display()));
                            continue;
                        }
                    };

                // Skip if hash already in media_files
                if crate::agent::rag::file_hash_exists(&ctx.pool, &hash)
                    .await
                    .unwrap_or(false)
                {
                    skipped_hash += 1;
                    continue;
                }

                // Extract raw metadata
                let path_for_meta = file_path.to_path_buf();
                let raw_meta = match tokio::task::spawn_blocking(move || {
                    crate::agent::metadata::extract(&path_for_meta)
                })
                .await?
                {
                    Ok(m) => m,
                    Err(e) => {
                        log.warn(&format!(
                            "Failed to extract metadata from {}: {e}",
                            file_path.display()
                        ));
                        continue;
                    }
                };

                // Parse path hints
                let relative = file_path.strip_prefix(inbox).unwrap_or(file_path);
                let uploader = crate::jobs::uploader_from_relative_path(&ctx.pool, relative).await;
                let hinted_relative = crate::jobs::strip_user_upload_prefix(relative);
                let hints = crate::agent::path_hints::parse(&hinted_relative);

                // Build context JSON
                let context = serde_json::json!({
                    "sha256": hash,
                    "file_size": file_size,
                    "raw_title": raw_meta.title,
                    "raw_artist": raw_meta.artist,
                    "raw_album": raw_meta.album,
                    "raw_track_number": raw_meta.track_number,
                    "raw_year": raw_meta.year,
                    "raw_genre": raw_meta.genre,
                    "duration_secs": raw_meta.duration_secs,
                    "audio_bitrate": raw_meta.audio_bitrate,
                    "audio_sample_rate": raw_meta.audio_sample_rate,
                    "audio_bit_depth": raw_meta.audio_bit_depth,
                    "uploaded_by_user_id": uploader.user_id,
                    "uploader_name": uploader.name,
                    "path_title": hints.title,
                    "path_artist": hints.artist,
                    "path_album": hints.album,
                    "path_year": hints.year,
                    "path_track_number": hints.track_number,
                });
                let context_str = serde_json::to_string(&context).unwrap_or_default();

                // Create PendingReview with status "queued"
                PendingReview::create_queued(
                    &ctx.db,
                    ctx.run_id,
                    "new_file",
                    Some(&input_path_str),
                    Some(&context_str),
                )
                .await
                .map_err(|e| anyhow::anyhow!("failed to create queued review: {e}"))?;

                discovered += 1;
            }
        }

        log.info(&format!(
            "Discovered {} new files, skipped {} (hash known), skipped {} (already queued)",
            discovered, skipped_hash, skipped_existing
        ));

        // Trigger inbox_process in background if new files were discovered
        // and no orchestrator is already running
        if discovered > 0 {
            if crate::jobs::inbox_process::is_orchestrator_running() {
                log.info(
                    "New files discovered but inbox_process already running, it will pick them up",
                );
            } else {
                log.info("Spawning inbox_process in background...");
                let config = ctx.config.clone();
                let db = ctx.db.clone();
                let pool = ctx.pool.clone();
                let registry = ctx.registry.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::scheduler::trigger_job_now(
                        &config,
                        &db,
                        &pool,
                        &registry,
                        "inbox_process",
                    )
                    .await
                    {
                        tracing::error!("Background inbox_process trigger failed: {e}");
                    }
                });
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers (moved from inbox_scan.rs)
// ---------------------------------------------------------------------------

pub fn group_by_folder(files: &[PathBuf]) -> Vec<(PathBuf, Vec<PathBuf>)> {
    use std::collections::HashMap;
    let mut map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for f in files {
        let folder = f.parent().unwrap_or(f).to_path_buf();
        map.entry(folder).or_default().push(f.clone());
    }
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = map.into_iter().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, files) in &mut groups {
        files.sort();
    }
    groups
}

pub async fn collect_audio_files(dir: &Path, audio: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let ft = entry.file_type().await?;
        if ft.is_dir() {
            Box::pin(collect_audio_files(&entry.path(), audio)).await?;
        } else if ft.is_file() && is_audio_file(&name) {
            audio.push(entry.path());
        }
    }
    Ok(())
}

pub fn is_audio_file(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    AUDIO_EXTENSIONS.contains(&ext.as_str())
}
