use std::path::{Path, PathBuf};

use crate::scheduler::{Job, JobContext, JobLog};

#[derive(Debug, Clone, Copy)]
pub struct MetadataBackfillOptions {
    pub audio_bitrate: bool,
    pub audio_sample_rate: bool,
    pub audio_bit_depth: bool,
    pub duration_seconds: bool,
    pub overwrite: bool,
}

impl MetadataBackfillOptions {
    pub fn any_field(self) -> bool {
        self.audio_bitrate
            || self.audio_sample_rate
            || self.audio_bit_depth
            || self.duration_seconds
    }
}

#[derive(sqlx::FromRow)]
struct BackfillRow {
    media_file_id: i64,
    file_path: String,
    audio_bitrate: Option<i32>,
    audio_sample_rate: Option<i32>,
    audio_bit_depth: Option<i32>,
    track_id: Option<i64>,
    duration_seconds: Option<f64>,
}

pub struct MetadataBackfillJob;

#[async_trait::async_trait]
impl Job for MetadataBackfillJob {
    fn name(&self) -> &'static str {
        "metadata_backfill"
    }

    fn description(&self) -> &'static str {
        "Backfill technical audio metadata from existing files"
    }

    fn default_cron(&self) -> &'static str {
        ""
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        run_with_options(
            ctx,
            log,
            MetadataBackfillOptions {
                audio_bitrate: true,
                audio_sample_rate: true,
                audio_bit_depth: true,
                duration_seconds: true,
                overwrite: false,
            },
        )
        .await
    }
}

pub async fn run_with_options(
    ctx: &JobContext,
    log: &mut JobLog,
    options: MetadataBackfillOptions,
) -> anyhow::Result<()> {
    if !options.any_field() {
        log.warn("No metadata fields selected; nothing to backfill");
        return Ok(());
    }

    let rows = sqlx::query_as::<_, BackfillRow>(
        "SELECT mf.id AS media_file_id, mf.file_path, \
                mf.audio_bitrate, mf.audio_sample_rate, mf.audio_bit_depth, \
                t.id AS track_id, t.duration_seconds \
         FROM furumusic__media_file mf \
         LEFT JOIN furumusic__track t ON t.audio_file_id = mf.id \
         WHERE mf.file_type = 'audio' \
         ORDER BY mf.id",
    )
    .fetch_all(&ctx.pool)
    .await?;

    log.info(&format!(
        "Metadata backfill started: {} audio file(s), mode={}",
        rows.len(),
        if options.overwrite {
            "overwrite"
        } else {
            "fill_missing"
        }
    ));

    let mut scanned = 0u64;
    let mut media_updated = 0u64;
    let mut track_updated = 0u64;
    let mut unchanged = 0u64;
    let mut missing = 0u64;
    let mut failed = 0u64;

    for row in rows {
        scanned += 1;
        let Some(path) = resolve_media_path(&row.file_path, &ctx.config.agent_storage_dir) else {
            missing += 1;
            log.warn(&format!("missing file: {}", row.file_path));
            continue;
        };

        let extract_path = path.clone();
        let raw_meta = match tokio::task::spawn_blocking(move || {
            crate::agent::metadata::extract(&extract_path)
        })
        .await
        {
            Ok(Ok(meta)) => meta,
            Ok(Err(e)) => {
                failed += 1;
                log.warn(&format!("metadata error for {}: {e}", path.display()));
                continue;
            }
            Err(e) => {
                failed += 1;
                log.warn(&format!("metadata task failed for {}: {e}", path.display()));
                continue;
            }
        };

        let mut changed_media = false;
        let mut next_bitrate = row.audio_bitrate;
        let mut next_sample_rate = row.audio_sample_rate;
        let mut next_bit_depth = row.audio_bit_depth;

        if options.audio_bitrate && should_update(row.audio_bitrate, options.overwrite) {
            if let Some(value) = raw_meta.audio_bitrate {
                next_bitrate = Some(value);
                changed_media = next_bitrate != row.audio_bitrate || changed_media;
            }
        }
        if options.audio_sample_rate && should_update(row.audio_sample_rate, options.overwrite) {
            if let Some(value) = raw_meta.audio_sample_rate {
                next_sample_rate = Some(value);
                changed_media = next_sample_rate != row.audio_sample_rate || changed_media;
            }
        }
        if options.audio_bit_depth && should_update(row.audio_bit_depth, options.overwrite) {
            if let Some(value) = raw_meta.audio_bit_depth {
                next_bit_depth = Some(value);
                changed_media = next_bit_depth != row.audio_bit_depth || changed_media;
            }
        }

        let mut changed_track = false;
        let mut next_duration = row.duration_seconds;
        if options.duration_seconds
            && row.track_id.is_some()
            && should_update_duration(row.duration_seconds, options.overwrite)
        {
            if let Some(value) = raw_meta.duration_secs {
                next_duration = Some(value);
                changed_track = row
                    .duration_seconds
                    .map(|current| (current - value).abs() > 0.001)
                    .unwrap_or(true);
            }
        }

        if changed_media {
            sqlx::query(
                "UPDATE furumusic__media_file \
                 SET audio_bitrate = $1, audio_sample_rate = $2, audio_bit_depth = $3 \
                 WHERE id = $4",
            )
            .bind(next_bitrate)
            .bind(next_sample_rate)
            .bind(next_bit_depth)
            .bind(row.media_file_id)
            .execute(&ctx.pool)
            .await?;
            media_updated += 1;
        }

        if changed_track {
            if let (Some(track_id), Some(duration)) = (row.track_id, next_duration) {
                sqlx::query("UPDATE furumusic__track SET duration_seconds = $1 WHERE id = $2")
                    .bind(duration)
                    .bind(track_id)
                    .execute(&ctx.pool)
                    .await?;
                track_updated += 1;
            }
        }

        if !changed_media && !changed_track {
            unchanged += 1;
        }

        if scanned % 100 == 0 {
            log.info(&format!(
                "Progress: {scanned} scanned, {media_updated} media updated, {track_updated} tracks updated, {unchanged} unchanged, {missing} missing, {failed} failed"
            ));
        }
    }

    log.info(&format!(
        "Metadata backfill complete: {scanned} scanned, {media_updated} media updated, {track_updated} tracks updated, {unchanged} unchanged, {missing} missing, {failed} failed"
    ));
    Ok(())
}

fn should_update<T>(current: Option<T>, overwrite: bool) -> bool {
    overwrite || current.is_none()
}

fn should_update_duration(current: Option<f64>, overwrite: bool) -> bool {
    overwrite || current.unwrap_or(0.0) <= 0.0
}

fn resolve_media_path(file_path: &str, storage_dir: &str) -> Option<PathBuf> {
    let path = Path::new(file_path);
    if path.exists() {
        return Some(path.to_path_buf());
    }
    if path.is_relative() && !storage_dir.is_empty() {
        let joined = Path::new(storage_dir).join(path);
        if joined.exists() {
            return Some(joined);
        }
    }
    None
}
