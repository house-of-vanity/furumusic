use std::path::{Path, PathBuf};

use crate::agent::cover_variants;
use crate::scheduler::{Job, JobContext, JobLog};

pub struct CoverVariantBackfillJob;

#[async_trait::async_trait]
impl Job for CoverVariantBackfillJob {
    fn name(&self) -> &'static str {
        "cover_variant_backfill"
    }

    fn description(&self) -> &'static str {
        "Generate missing resized cover image variants"
    }

    fn default_cron(&self) -> &'static str {
        // Once a day after cover extraction and artist image assignment.
        "0 45 3 * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        let storage_dir = &ctx.config.agent_storage_dir;
        if storage_dir.is_empty() {
            log.warn("agent_storage_dir is not configured, skipping cover variant backfill");
            return Ok(());
        }

        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, file_path FROM furumusic__media_file WHERE file_type = 'cover_art' ORDER BY id",
        )
        .fetch_all(&ctx.pool)
        .await?;

        if rows.is_empty() {
            log.info("No cover art media files found");
            return Ok(());
        }

        log.info(&format!(
            "Found {} cover art media file(s), checking variants...",
            rows.len()
        ));

        let mut created = 0usize;
        let mut unchanged = 0usize;
        let mut missing_original = 0usize;
        let mut failed = 0usize;

        for (media_file_id, file_path) in rows {
            let path = resolve_media_path(storage_dir, &file_path);
            if !path.exists() {
                missing_original += 1;
                log.warn(&format!(
                    "Media file {media_file_id}: original cover not found at {}",
                    path.display()
                ));
                continue;
            }

            match cover_variants::ensure_cover_variants(&path).await {
                Ok(0) => unchanged += 1,
                Ok(count) => {
                    created += count;
                    log.info(&format!(
                        "Media file {media_file_id}: created {count} variant(s)"
                    ));
                }
                Err(err) => {
                    failed += 1;
                    log.warn(&format!(
                        "Media file {media_file_id}: failed to create variants: {err}"
                    ));
                }
            }
        }

        log.info(&format!(
            "Cover variant backfill complete: {created} variant(s) created, \
             {unchanged} original(s) already complete, {missing_original} missing original(s), \
             {failed} failed original(s)"
        ));

        Ok(())
    }
}

fn resolve_media_path(storage_dir: &str, file_path: &str) -> PathBuf {
    let path = PathBuf::from(file_path);
    if path.is_absolute() {
        path
    } else {
        Path::new(storage_dir).join(path)
    }
}
