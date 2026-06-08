use std::collections::BTreeSet;
use std::io::ErrorKind;

use sqlx::PgPool;

use crate::scheduler::{Job, JobContext, JobLog};

const SAMPLE_LOG_LIMIT: usize = 50;

pub struct ArchiveCleanupJob;

#[derive(Debug, sqlx::FromRow)]
struct TrackFileRow {
    track_id: i64,
    track_title: String,
    release_id: i64,
    release_title: Option<String>,
    media_file_id: Option<i64>,
    file_type: Option<String>,
    file_path: Option<String>,
}

#[derive(Debug)]
struct MissingTrack {
    track_id: i64,
    track_title: String,
    release_id: i64,
    release_title: Option<String>,
    media_file_id: Option<i64>,
    file_path: Option<String>,
    reason: MissingReason,
}

#[derive(Debug)]
enum MissingReason {
    MissingMediaRow,
    InvalidMediaType(String),
    EmptyPath,
    MissingFile,
    NotRegularFile,
}

#[derive(Debug, Default)]
struct DeleteStats {
    playback_states_cleared: u64,
    playlist_entries_deleted: u64,
    likes_deleted: u64,
    play_history_deleted: u64,
    popularity_history_deleted: u64,
    scrobble_outbox_deleted: u64,
    track_genres_deleted: u64,
    entity_tags_deleted: u64,
    external_ids_deleted: u64,
    track_artists_deleted: u64,
    tracks_deleted: u64,
    media_files_deleted: u64,
}

#[async_trait::async_trait]
impl Job for ArchiveCleanupJob {
    fn name(&self) -> &'static str {
        "archive_cleanup"
    }

    fn description(&self) -> &'static str {
        "Clean stale archive records, starting with tracks whose audio files are missing"
    }

    fn default_cron(&self) -> &'static str {
        // Daily at 04:45.
        "0 45 4 * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        run_missing_audio_cleanup(ctx, log).await
    }
}

async fn run_missing_audio_cleanup(ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
    let storage_dir = ctx.config.agent_storage_dir.trim();
    if storage_dir.is_empty() {
        log.warn("Archive cleanup: agent_storage_dir is not configured, skipping file checks");
        return Ok(());
    }

    let rows = sqlx::query_as::<_, TrackFileRow>(
        r#"SELECT t.id AS track_id,
                  t.title::text AS track_title,
                  t.release_id,
                  r.title::text AS release_title,
                  mf.id AS media_file_id,
                  mf.file_type::text AS file_type,
                  mf.file_path::text AS file_path
             FROM furumusic__track t
             LEFT JOIN furumusic__release r ON r.id = t.release_id
             LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
            ORDER BY t.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    if rows.is_empty() {
        log.info("Archive cleanup: no tracks found");
        return Ok(());
    }

    log.info(&format!(
        "Archive cleanup: checking {} track audio reference(s)",
        rows.len()
    ));

    let mut missing_tracks = Vec::new();
    let mut skipped_io_errors = 0u64;

    for row in rows {
        let Some(media_file_id) = row.media_file_id else {
            missing_tracks.push(MissingTrack::from_row(row, MissingReason::MissingMediaRow));
            continue;
        };

        let file_type = row.file_type.clone();
        match file_type.as_deref() {
            Some("audio") => {}
            Some(file_type) => {
                missing_tracks.push(MissingTrack::from_row(
                    row,
                    MissingReason::InvalidMediaType(file_type.to_owned()),
                ));
                continue;
            }
            None => {
                missing_tracks.push(MissingTrack::from_row(row, MissingReason::MissingMediaRow));
                continue;
            }
        }

        let Some(file_path) = row
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            missing_tracks.push(MissingTrack::from_row(row, MissingReason::EmptyPath));
            continue;
        };

        let absolute_path = crate::media_paths::resolve_media_file_path(storage_dir, file_path);
        match tokio::fs::metadata(&absolute_path).await {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                missing_tracks.push(MissingTrack::from_row(row, MissingReason::NotRegularFile));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                missing_tracks.push(MissingTrack::from_row(row, MissingReason::MissingFile));
            }
            Err(err) => {
                skipped_io_errors += 1;
                log.warn(&format!(
                    "Archive cleanup: skipping track {} media_file_id={media_file_id}; cannot inspect {}: {err}",
                    row.track_id,
                    absolute_path.display()
                ));
            }
        }
    }

    if missing_tracks.is_empty() {
        log.info(&format!(
            "Archive cleanup: all checked tracks have readable audio files; skipped_io_errors={skipped_io_errors}"
        ));
        return Ok(());
    }

    for (index, track) in missing_tracks.iter().take(SAMPLE_LOG_LIMIT).enumerate() {
        log.warn(&format!(
            "Archive cleanup: deleting stale track {} \"{}\"{}{} ({})",
            track.track_id,
            track.track_title,
            track
                .release_title
                .as_deref()
                .map(|title| format!(" from \"{title}\""))
                .unwrap_or_default(),
            track
                .file_path
                .as_deref()
                .map(|path| format!(", path={path}"))
                .unwrap_or_default(),
            track.reason
        ));
        if index + 1 == SAMPLE_LOG_LIMIT && missing_tracks.len() > SAMPLE_LOG_LIMIT {
            log.warn(&format!(
                "Archive cleanup: suppressing per-track logs for remaining {} stale track(s)",
                missing_tracks.len() - SAMPLE_LOG_LIMIT
            ));
        }
    }

    let track_ids = unique_sorted(
        missing_tracks
            .iter()
            .map(|track| track.track_id)
            .collect::<Vec<_>>(),
    );
    let media_file_ids = unique_sorted(
        missing_tracks
            .iter()
            .filter_map(|track| track.media_file_id)
            .collect::<Vec<_>>(),
    );
    let release_ids = unique_sorted(
        missing_tracks
            .iter()
            .map(|track| track.release_id)
            .collect::<Vec<_>>(),
    );

    let stats =
        delete_tracks_and_unreferenced_audio_media(&ctx.pool, &track_ids, &media_file_ids).await?;
    let empty_release_count = count_empty_releases(&ctx.pool, &release_ids).await?;

    log.info(&format!(
        "Archive cleanup: deleted {} track(s), {} unreferenced audio media_file row(s); cleared playback_states={}, playlist_entries={}, likes={}, play_history={}, popularity_history={}, scrobble_outbox={}, track_genres={}, entity_tags={}, external_ids={}, track_artists={}; skipped_io_errors={skipped_io_errors}; empty_releases_left={empty_release_count}",
        stats.tracks_deleted,
        stats.media_files_deleted,
        stats.playback_states_cleared,
        stats.playlist_entries_deleted,
        stats.likes_deleted,
        stats.play_history_deleted,
        stats.popularity_history_deleted,
        stats.scrobble_outbox_deleted,
        stats.track_genres_deleted,
        stats.entity_tags_deleted,
        stats.external_ids_deleted,
        stats.track_artists_deleted,
    ));

    Ok(())
}

impl MissingTrack {
    fn from_row(row: TrackFileRow, reason: MissingReason) -> Self {
        Self {
            track_id: row.track_id,
            track_title: row.track_title,
            release_id: row.release_id,
            release_title: row.release_title,
            media_file_id: row.media_file_id,
            file_path: row.file_path,
            reason,
        }
    }
}

impl std::fmt::Display for MissingReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMediaRow => f.write_str("missing media_file row"),
            Self::InvalidMediaType(file_type) => write!(f, "invalid media_file type {file_type:?}"),
            Self::EmptyPath => f.write_str("empty media file path"),
            Self::MissingFile => f.write_str("audio file not found on disk"),
            Self::NotRegularFile => f.write_str("audio path is not a regular file"),
        }
    }
}

fn unique_sorted(values: Vec<i64>) -> Vec<i64> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn delete_tracks_and_unreferenced_audio_media(
    pool: &PgPool,
    track_ids: &[i64],
    media_file_ids: &[i64],
) -> anyhow::Result<DeleteStats> {
    if track_ids.is_empty() {
        return Ok(DeleteStats::default());
    }

    let mut tx = pool.begin().await?;
    let mut stats = DeleteStats::default();

    stats.playback_states_cleared = sqlx::query(
        r#"UPDATE furumusic__playback_state
              SET current_track_id = NULL
            WHERE current_track_id = ANY($1)"#,
    )
    .bind(track_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    stats.playlist_entries_deleted =
        delete_track_rows(&mut tx, "furumusic__playlist_track", track_ids).await?;
    stats.likes_deleted =
        delete_track_rows(&mut tx, "furumusic__user_liked_track", track_ids).await?;
    stats.play_history_deleted =
        delete_track_rows(&mut tx, "furumusic__play_history", track_ids).await?;
    stats.popularity_history_deleted =
        delete_track_rows(&mut tx, "furumusic__track_popularity_history", track_ids).await?;
    stats.scrobble_outbox_deleted =
        delete_track_rows(&mut tx, "furumusic__lastfm_scrobble_outbox", track_ids).await?;
    stats.track_genres_deleted =
        delete_track_rows(&mut tx, "furumusic__track_genre", track_ids).await?;

    stats.entity_tags_deleted = sqlx::query(
        r#"DELETE FROM furumusic__entity_genre_tag
            WHERE entity_kind = 'track'
              AND entity_id = ANY($1)"#,
    )
    .bind(track_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    stats.external_ids_deleted = sqlx::query(
        r#"DELETE FROM furumusic__external_metadata_id
            WHERE entity_kind = 'track'
              AND entity_id = ANY($1)"#,
    )
    .bind(track_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    stats.track_artists_deleted =
        delete_track_rows(&mut tx, "furumusic__track_artist", track_ids).await?;

    stats.tracks_deleted = sqlx::query("DELETE FROM furumusic__track WHERE id = ANY($1)")
        .bind(track_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if !media_file_ids.is_empty() {
        stats.media_files_deleted = sqlx::query(
            r#"DELETE FROM furumusic__media_file mf
                WHERE mf.id = ANY($1)
                  AND mf.file_type = 'audio'
                  AND NOT EXISTS (
                        SELECT 1
                          FROM furumusic__track t
                         WHERE t.audio_file_id = mf.id
                            OR t.cover_file_id = mf.id
                  )
                  AND NOT EXISTS (
                        SELECT 1
                          FROM furumusic__release r
                         WHERE r.cover_file_id = mf.id
                  )
                  AND NOT EXISTS (
                        SELECT 1
                          FROM furumusic__artist a
                         WHERE a.image_file_id = mf.id
                  )
                  AND NOT EXISTS (
                        SELECT 1
                          FROM furumusic__playlist p
                         WHERE p.cover_file_id = mf.id
                  )"#,
        )
        .bind(media_file_ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }

    tx.commit().await?;
    Ok(stats)
}

async fn delete_track_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    track_ids: &[i64],
) -> anyhow::Result<u64> {
    let sql = format!("DELETE FROM {table} WHERE track_id = ANY($1)");
    Ok(sqlx::query(&sql)
        .bind(track_ids)
        .execute(&mut **tx)
        .await?
        .rows_affected())
}

async fn count_empty_releases(pool: &PgPool, release_ids: &[i64]) -> anyhow::Result<i64> {
    if release_ids.is_empty() {
        return Ok(0);
    }

    let count = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)
             FROM furumusic__release r
            WHERE r.id = ANY($1)
              AND NOT EXISTS (
                    SELECT 1
                      FROM furumusic__track t
                     WHERE t.release_id = r.id
              )"#,
    )
    .bind(release_ids)
    .fetch_one(pool)
    .await?;

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_sorted_deduplicates_ids() {
        assert_eq!(unique_sorted(vec![3, 1, 3, 2, 1]), vec![1, 2, 3]);
    }

    #[test]
    fn missing_reason_display_is_stable() {
        assert_eq!(
            MissingReason::InvalidMediaType("cover_art".to_owned()).to_string(),
            "invalid media_file type \"cover_art\""
        );
        assert_eq!(
            MissingReason::MissingFile.to_string(),
            "audio file not found on disk"
        );
    }
}
