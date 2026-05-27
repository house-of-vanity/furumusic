use crate::lastfm;
use crate::scheduler::{Job, JobContext, JobLog};

pub struct LastfmScrobbleJob;

#[async_trait::async_trait]
impl Job for LastfmScrobbleJob {
    fn name(&self) -> &'static str {
        "lastfm_scrobble"
    }

    fn description(&self) -> &'static str {
        "Send queued Last.fm scrobbles for connected users"
    }

    fn default_cron(&self) -> &'static str {
        // Every minute.
        "0 * * * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        if !lastfm::is_configured(&ctx.config) {
            log.warn("Last.fm API key/shared secret are not configured; skipping scrobble outbox");
            return Ok(());
        }

        let summary = lastfm::process_pending_scrobbles(&ctx.pool, &ctx.config, None, 50).await?;
        log.info(&format!(
            "Last.fm scrobble outbox processed: considered={}, sent={}, failed={}, blocked={}, skipped={}",
            summary.considered, summary.sent, summary.failed, summary.blocked, summary.skipped
        ));
        Ok(())
    }
}
