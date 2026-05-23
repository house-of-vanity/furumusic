use crate::scheduler::{Job, JobContext, JobLog};

/// Fallback job that assigns artist images from track cover art.
///
/// The primary `artist_image_backfill` job uses release covers.  This job
/// runs afterwards and covers the case where the release itself has no
/// cover but individual tracks do (e.g. when cover art is embedded in the
/// audio file and extracted per-track rather than per-release).
///
/// For every artist that *still* has no `image_file_id` after the release-
/// based backfill, picks the `cover_file_id` of the most recent track
/// (by year, then track id) that has one.
pub struct ArtistTrackImageBackfillJob;

#[async_trait::async_trait]
impl Job for ArtistTrackImageBackfillJob {
    fn name(&self) -> &'static str {
        "artist_track_image_backfill"
    }

    fn description(&self) -> &'static str {
        "Auto-assign artist images from track covers (fallback)"
    }

    fn default_cron(&self) -> &'static str {
        // 03:30 daily — after artist_image_backfill at 03:15
        "0 30 3 * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE furumusic__artist a \
             SET image_file_id = ( \
                 SELECT t.cover_file_id \
                 FROM furumusic__track_artist ta \
                 JOIN furumusic__track t ON t.id = ta.track_id \
                 WHERE ta.artist_id = a.id \
                   AND t.cover_file_id IS NOT NULL \
                   AND t.is_hidden = false \
                 ORDER BY t.year DESC NULLS LAST, t.id DESC \
                 LIMIT 1 \
             ), \
             updated_at = $1 \
             WHERE a.image_file_id IS NULL \
               AND a.is_hidden = false \
               AND EXISTS ( \
                   SELECT 1 FROM furumusic__track_artist ta2 \
                   JOIN furumusic__track t2 ON t2.id = ta2.track_id \
                   WHERE ta2.artist_id = a.id \
                     AND t2.cover_file_id IS NOT NULL \
                     AND t2.is_hidden = false \
               )",
        )
        .bind(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .execute(&ctx.pool)
        .await?;

        let count = result.rows_affected();
        if count > 0 {
            log.info(&format!(
                "Assigned images to {count} artists from track covers"
            ));
        } else {
            log.info("All artists already have images (or no track covers available)");
        }

        Ok(())
    }
}
