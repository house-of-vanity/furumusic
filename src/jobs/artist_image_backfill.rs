use crate::scheduler::{Job, JobContext, JobLog};

/// Periodic job that auto-assigns artist images from their release covers.
///
/// For every artist that has no `image_file_id`, picks the cover of the most
/// recent release (by year) that has one. Runs after the cover backfill job
/// so freshly-extracted covers are available.
pub struct ArtistImageBackfillJob;

#[async_trait::async_trait]
impl Job for ArtistImageBackfillJob {
    fn name(&self) -> &'static str {
        "artist_image_backfill"
    }

    fn description(&self) -> &'static str {
        "Auto-assign artist images from release covers"
    }

    fn default_cron(&self) -> &'static str {
        // 03:15 daily — after cover_backfill at 03:00
        "0 15 3 * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE furumusic__artist a \
             SET image_file_id = ( \
                 SELECT r.cover_file_id \
                 FROM furumusic__release_artist ra \
                 JOIN furumusic__release r ON r.id = ra.release_id \
                 WHERE ra.artist_id = a.id \
                   AND r.cover_file_id IS NOT NULL \
                 ORDER BY r.year DESC NULLS LAST \
                 LIMIT 1 \
             ), \
             updated_at = $1 \
             WHERE a.image_file_id IS NULL \
               AND EXISTS ( \
                   SELECT 1 FROM furumusic__release_artist ra2 \
                   JOIN furumusic__release r2 ON r2.id = ra2.release_id \
                   WHERE ra2.artist_id = a.id AND r2.cover_file_id IS NOT NULL \
               )",
        )
        .bind(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .execute(&ctx.pool)
        .await?;

        let count = result.rows_affected();
        if count > 0 {
            log.info(&format!(
                "Assigned images to {count} artists from release covers"
            ));
        } else {
            log.info("All artists already have images (or no covers available)");
        }

        Ok(())
    }
}
