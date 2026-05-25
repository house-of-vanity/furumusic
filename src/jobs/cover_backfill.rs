use std::path::{Path, PathBuf};

use crate::agent::cover_art;
use crate::scheduler::{Job, JobContext, JobLog};

/// One-shot / periodic job that finds releases without cover art and attempts
/// to extract or discover covers from their audio files in storage.
pub struct CoverBackfillJob;

#[async_trait::async_trait]
impl Job for CoverBackfillJob {
    fn name(&self) -> &'static str {
        "cover_backfill"
    }

    fn description(&self) -> &'static str {
        "Backfill cover art for releases missing covers"
    }

    fn default_cron(&self) -> &'static str {
        // Once a day at 03:00
        "0 0 3 * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        let storage_dir = &ctx.config.agent_storage_dir;
        if storage_dir.is_empty() {
            log.warn("agent_storage_dir is not configured, skipping cover backfill");
            return Ok(());
        }

        // Find all releases without a cover
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT r.id, r.title \
             FROM furumusic__release r \
             WHERE r.cover_file_id IS NULL \
             ORDER BY r.id",
        )
        .fetch_all(&ctx.pool)
        .await?;

        if rows.is_empty() {
            log.info("All releases already have cover art, nothing to backfill");
            return Ok(());
        }

        log.info(&format!(
            "Found {} releases without cover art, starting backfill...",
            rows.len()
        ));

        let mut assigned = 0u32;
        let mut failed = 0u32;
        let mut skipped_no_audio = 0u32;
        let mut skipped_no_cover = 0u32;
        let total = rows.len();

        for (i, (release_id, release_title)) in rows.iter().enumerate() {
            log.info(&format!(
                "[{}/{}] Processing release {release_id} \"{release_title}\"...",
                i + 1,
                total,
            ));

            // Find audio files belonging to this release via tracks → media_file
            let audio_paths: Vec<(String,)> = sqlx::query_as(
                "SELECT mf.file_path \
                 FROM furumusic__track t \
                 JOIN furumusic__media_file mf ON mf.id = t.audio_file_id \
                 WHERE t.release_id = $1 AND mf.file_type = 'audio'",
            )
            .bind(release_id)
            .fetch_all(&ctx.pool)
            .await
            .unwrap_or_default();

            if audio_paths.is_empty() {
                log.warn(&format!(
                    "Release {release_id} \"{release_title}\": no audio files found, skipping"
                ));
                skipped_no_audio += 1;
                continue;
            }

            // Determine the folder from the first audio file's path
            let first_path = Path::new(&audio_paths[0].0);
            let folder = first_path.parent().unwrap_or(Path::new("."));

            // Collect all audio file paths as PathBuf
            let audio_files: Vec<PathBuf> =
                audio_paths.iter().map(|(p,)| PathBuf::from(p)).collect();

            // Try to find cover art
            let cover = match cover_art::find_best_cover(folder, &audio_files).await {
                Some(c) => c,
                None => {
                    log.info(&format!(
                        "Release {release_id} \"{release_title}\": no cover image found in {} audio files, skipping",
                        audio_files.len(),
                    ));
                    skipped_no_cover += 1;
                    continue;
                }
            };

            let source_desc = match &cover.source {
                cover_art::CoverSource::FolderFile(p) => format!("folder: {}", p.display()),
                cover_art::CoverSource::Embedded(p) => format!("embedded: {}", p.display()),
            };

            // Look up artist name for storage path
            let artist_name: String = sqlx::query_scalar(
                "SELECT a.name FROM furumusic__artist a \
                 JOIN furumusic__release_artist ra ON ra.artist_id = a.id \
                 WHERE ra.release_id = $1 \
                 ORDER BY ra.position LIMIT 1",
            )
            .bind(release_id)
            .fetch_optional(&ctx.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "Unknown Artist".to_string());

            match cover_art::save_cover_to_storage(
                &ctx.db,
                &ctx.pool,
                storage_dir,
                &artist_name,
                release_title,
                &cover,
            )
            .await
            {
                Ok(cover_file_id) => {
                    if let Err(e) =
                        cover_art::assign_cover_to_release(&ctx.pool, *release_id, cover_file_id)
                            .await
                    {
                        log.warn(&format!(
                            "Release {release_id} \"{release_title}\": saved cover but failed to assign: {e}"
                        ));
                        failed += 1;
                    } else {
                        log.info(&format!(
                            "Release {release_id} \"{release_title}\": assigned cover from {source_desc}"
                        ));
                        assigned += 1;
                    }
                }
                Err(e) => {
                    log.warn(&format!(
                        "Release {release_id} \"{release_title}\": failed to save cover: {e}"
                    ));
                    failed += 1;
                }
            }
        }

        log.info(&format!(
            "Cover backfill complete: {assigned} assigned, {failed} failed, \
             {skipped_no_audio} skipped (no audio), {skipped_no_cover} skipped (no cover found)"
        ));

        Ok(())
    }
}
