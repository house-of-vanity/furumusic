use std::path::PathBuf;

use reqwest::Client;
use serde::{
    Deserialize,
    de::{self, DeserializeOwned},
};

use crate::agent::cover_art::{self, CoverImage, CoverSource};
use crate::agent::cover_variants;
use crate::scheduler::{Job, JobContext, JobLog};

pub struct ArtworkBackfillJob;

const LASTFM_REQUEST_DELAY: std::time::Duration = std::time::Duration::from_millis(1200);
const MAX_LASTFM_RELEASE_LOOKUPS: i64 = 200;
const MAX_LASTFM_ARTIST_LOOKUPS: i64 = 200;

#[derive(Debug, sqlx::FromRow)]
struct ReleaseCandidate {
    id: i64,
    title: String,
    artist_name: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ArtistCandidate {
    id: i64,
    name: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ArtworkRefCandidate {
    entity_id: i64,
    media_file_id: i64,
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmAlbumResponse {
    album: Option<LastfmImageContainer>,
    error: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmArtistResponse {
    artist: Option<LastfmImageContainer>,
    error: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmTopAlbumsResponse {
    topalbums: Option<LastfmTopAlbumsContainer>,
    error: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmTopAlbumsContainer {
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    album: Vec<LastfmTopAlbum>,
}

#[derive(Debug, Deserialize)]
struct LastfmTopAlbum {
    image: Option<Vec<LastfmImage>>,
}

#[derive(Debug, Deserialize)]
struct LastfmImageContainer {
    image: Option<Vec<LastfmImage>>,
}

#[derive(Debug, Deserialize)]
struct LastfmImage {
    #[serde(rename = "#text")]
    url: String,
    size: String,
}

#[derive(Default)]
struct ArtworkStats {
    broken_release_refs_cleared: u64,
    broken_track_refs_cleared: u64,
    broken_artist_refs_cleared: u64,
    release_local_assigned: u64,
    release_lastfm_assigned: u64,
    release_lastfm_not_found: u64,
    release_skipped_no_audio: u64,
    artist_lastfm_assigned: u64,
    artist_lastfm_not_found: u64,
    artist_album_fallback_assigned: u64,
    variants_created: usize,
    variants_unchanged: usize,
    variants_missing_original: usize,
    failed: u64,
}

#[async_trait::async_trait]
impl Job for ArtworkBackfillJob {
    fn name(&self) -> &'static str {
        "artwork_backfill"
    }

    fn description(&self) -> &'static str {
        "Backfill and repair release, track, and artist artwork"
    }

    fn default_cron(&self) -> &'static str {
        // Nightly, after inbox processing has had a chance to import new files.
        "0 30 3 * * *"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        let storage_dir = ctx.config.agent_storage_dir.trim();
        if storage_dir.is_empty() {
            log.warn("agent_storage_dir is not configured, skipping artwork backfill");
            return Ok(());
        }

        let client = Client::builder()
            .user_agent(format!(
                "furumusic-artwork-backfill/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(20))
            .build()?;
        let mut stats = ArtworkStats::default();

        let normalized_paths =
            crate::media_paths::normalize_media_file_paths(&ctx.pool, storage_dir).await?;
        if normalized_paths > 0 {
            log.info(&format!(
                "Media path normalization pass: rewrote {normalized_paths} media file path(s) to relative storage paths"
            ));
        } else {
            log.info("Media path normalization pass: all media file paths are already relative");
        }

        repair_missing_artwork_refs(ctx, log, storage_dir, &mut stats).await?;
        backfill_release_local(ctx, log, storage_dir, &mut stats).await?;

        let api_key = ctx.config.lastfm_api_key.trim();
        if api_key.is_empty() {
            log.warn("lastfm_api_key is not configured; skipping Last.fm artwork fallback");
        } else {
            backfill_release_lastfm(ctx, log, storage_dir, api_key, &client, &mut stats).await?;
            backfill_artist_lastfm(ctx, log, storage_dir, api_key, &client, &mut stats).await?;
        }

        backfill_artist_album_fallbacks(ctx, log, &mut stats).await?;
        repair_cover_variants(ctx, log, storage_dir, &mut stats).await?;

        log.info(&format!(
            "Artwork backfill complete: broken_release_refs_cleared={}, broken_track_refs_cleared={}, broken_artist_refs_cleared={}, release_local_assigned={}, release_lastfm_assigned={}, release_lastfm_not_found={}, release_skipped_no_audio={}, artist_lastfm_assigned={}, artist_lastfm_not_found={}, artist_album_fallback_assigned={}, variants_created={}, variants_unchanged={}, variants_missing_original={}, failed={}",
            stats.broken_release_refs_cleared,
            stats.broken_track_refs_cleared,
            stats.broken_artist_refs_cleared,
            stats.release_local_assigned,
            stats.release_lastfm_assigned,
            stats.release_lastfm_not_found,
            stats.release_skipped_no_audio,
            stats.artist_lastfm_assigned,
            stats.artist_lastfm_not_found,
            stats.artist_album_fallback_assigned,
            stats.variants_created,
            stats.variants_unchanged,
            stats.variants_missing_original,
            stats.failed
        ));
        Ok(())
    }
}

async fn repair_missing_artwork_refs(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    repair_missing_release_cover_refs(ctx, log, storage_dir, stats).await?;
    repair_missing_track_cover_refs(ctx, log, storage_dir, stats).await?;
    repair_missing_artist_image_refs(ctx, log, storage_dir, stats).await?;
    Ok(())
}

async fn repair_missing_release_cover_refs(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, ArtworkRefCandidate>(
        r#"SELECT r.id AS entity_id,
                  r.cover_file_id AS media_file_id,
                  mf.file_path::text AS file_path
             FROM furumusic__release r
             LEFT JOIN furumusic__media_file mf ON mf.id = r.cover_file_id
            WHERE r.cover_file_id IS NOT NULL
              AND r.is_hidden = false
            ORDER BY r.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    for row in rows {
        if artwork_ref_exists(storage_dir, row.file_path.as_deref()) {
            continue;
        }

        let result = sqlx::query(
            r#"UPDATE furumusic__release
                  SET cover_file_id = NULL,
                      updated_at = $3
                WHERE id = $1
                  AND cover_file_id = $2"#,
        )
        .bind(row.entity_id)
        .bind(row.media_file_id)
        .bind(now_iso())
        .execute(&ctx.pool)
        .await?;

        if result.rows_affected() > 0 {
            reset_lookup_state(&ctx.pool, "release", row.entity_id).await?;
            stats.broken_release_refs_cleared += 1;
            log.warn(&format!(
                "Release {}: cleared missing cover reference media_file_id={}{}",
                row.entity_id,
                row.media_file_id,
                artwork_ref_location(storage_dir, row.file_path.as_deref())
            ));
        }
    }

    Ok(())
}

async fn repair_missing_track_cover_refs(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, ArtworkRefCandidate>(
        r#"SELECT t.id AS entity_id,
                  t.cover_file_id AS media_file_id,
                  mf.file_path::text AS file_path
             FROM furumusic__track t
             LEFT JOIN furumusic__media_file mf ON mf.id = t.cover_file_id
            WHERE t.cover_file_id IS NOT NULL
              AND t.is_hidden = false
            ORDER BY t.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    for row in rows {
        if artwork_ref_exists(storage_dir, row.file_path.as_deref()) {
            continue;
        }

        let result = sqlx::query(
            r#"UPDATE furumusic__track
                  SET cover_file_id = NULL,
                      updated_at = $3
                WHERE id = $1
                  AND cover_file_id = $2"#,
        )
        .bind(row.entity_id)
        .bind(row.media_file_id)
        .bind(now_iso())
        .execute(&ctx.pool)
        .await?;

        if result.rows_affected() > 0 {
            stats.broken_track_refs_cleared += 1;
            log.warn(&format!(
                "Track {}: cleared missing cover reference media_file_id={}{}",
                row.entity_id,
                row.media_file_id,
                artwork_ref_location(storage_dir, row.file_path.as_deref())
            ));
        }
    }

    Ok(())
}

async fn repair_missing_artist_image_refs(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, ArtworkRefCandidate>(
        r#"SELECT a.id AS entity_id,
                  a.image_file_id AS media_file_id,
                  mf.file_path::text AS file_path
             FROM furumusic__artist a
             LEFT JOIN furumusic__media_file mf ON mf.id = a.image_file_id
            WHERE a.image_file_id IS NOT NULL
              AND a.is_hidden = false
            ORDER BY a.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    for row in rows {
        if artwork_ref_exists(storage_dir, row.file_path.as_deref()) {
            continue;
        }

        let result = sqlx::query(
            r#"UPDATE furumusic__artist
                  SET image_file_id = NULL,
                      updated_at = $3
                WHERE id = $1
                  AND image_file_id = $2"#,
        )
        .bind(row.entity_id)
        .bind(row.media_file_id)
        .bind(now_iso())
        .execute(&ctx.pool)
        .await?;

        if result.rows_affected() > 0 {
            reset_lookup_state(&ctx.pool, "artist", row.entity_id).await?;
            stats.broken_artist_refs_cleared += 1;
            log.warn(&format!(
                "Artist {}: cleared missing image reference media_file_id={}{}",
                row.entity_id,
                row.media_file_id,
                artwork_ref_location(storage_dir, row.file_path.as_deref())
            ));
        }
    }

    Ok(())
}

fn artwork_ref_exists(storage_dir: &str, file_path: Option<&str>) -> bool {
    file_path
        .map(|value| crate::media_paths::resolve_media_file_path(storage_dir, value).exists())
        .unwrap_or(false)
}

fn artwork_ref_location(storage_dir: &str, file_path: Option<&str>) -> String {
    file_path
        .map(|value| {
            let path = crate::media_paths::resolve_media_file_path(storage_dir, value);
            format!(" at {}", path.display())
        })
        .unwrap_or_else(|| " with missing media_file row".to_string())
}

async fn backfill_release_local(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let releases = sqlx::query_as::<_, ReleaseCandidate>(
        r#"SELECT r.id,
                  r.title::text AS title,
                  (
                    SELECT a.name::text
                      FROM furumusic__release_artist ra
                      JOIN furumusic__artist a ON a.id = ra.artist_id
                     WHERE ra.release_id = r.id
                     ORDER BY ra.position
                     LIMIT 1
                  ) AS artist_name
             FROM furumusic__release r
            WHERE r.cover_file_id IS NULL
              AND r.is_hidden = false
            ORDER BY r.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    if releases.is_empty() {
        log.info("Release local artwork pass: all visible releases already have covers");
        return Ok(());
    }
    log.info(&format!(
        "Release local artwork pass: checking {} release(s) without covers",
        releases.len()
    ));

    for (index, release) in releases.iter().enumerate() {
        log.info(&format!(
            "Release local artwork {}/{}: release {} \"{}\"",
            index + 1,
            releases.len(),
            release.id,
            release.title
        ));

        let audio_paths: Vec<String> = sqlx::query_scalar(
            r#"SELECT mf.file_path::text
                 FROM furumusic__track t
                 JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
                WHERE t.release_id = $1
                  AND mf.file_type = 'audio'
                ORDER BY t.disc_number NULLS LAST, t.track_number NULLS LAST, t.id"#,
        )
        .bind(release.id)
        .fetch_all(&ctx.pool)
        .await
        .unwrap_or_default();

        if audio_paths.is_empty() {
            stats.release_skipped_no_audio += 1;
            log.warn(&format!(
                "Release {} \"{}\": no audio files found for local cover extraction",
                release.id, release.title
            ));
            continue;
        }

        let audio_files: Vec<PathBuf> = audio_paths
            .iter()
            .map(|path| crate::media_paths::resolve_media_file_path(storage_dir, path))
            .collect();
        let Some(folder) = audio_files.first().and_then(|path| path.parent()) else {
            stats.failed += 1;
            log.warn(&format!(
                "Release {} \"{}\": could not determine audio folder",
                release.id, release.title
            ));
            continue;
        };

        let Some(cover) = cover_art::find_best_cover(folder, &audio_files).await else {
            continue;
        };

        let source_desc = cover_source_description(&cover.source);
        let artist_name = release.artist_name.as_deref().unwrap_or("Unknown Artist");
        match cover_art::save_cover_to_storage(
            &ctx.db,
            &ctx.pool,
            storage_dir,
            artist_name,
            &release.title,
            &cover,
        )
        .await
        {
            Ok(cover_file_id) => {
                cover_art::assign_cover_to_release(&ctx.pool, release.id, cover_file_id).await?;
                stats.release_local_assigned += 1;
                log.info(&format!(
                    "Release {} \"{}\": assigned local cover from {source_desc}",
                    release.id, release.title
                ));
            }
            Err(err) => {
                stats.failed += 1;
                log.warn(&format!(
                    "Release {} \"{}\": failed to save local cover: {err}",
                    release.id, release.title
                ));
            }
        }
    }

    Ok(())
}

async fn backfill_release_lastfm(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    api_key: &str,
    client: &Client,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let failed_cutoff = cutoff_iso(1);
    let not_found_cutoff = cutoff_iso(30);
    let releases = sqlx::query_as::<_, ReleaseCandidate>(
        r#"SELECT r.id,
                  r.title::text AS title,
                  COALESCE(
                    (
                      SELECT a.name::text
                        FROM furumusic__release_artist ra
                        JOIN furumusic__artist a ON a.id = ra.artist_id
                       WHERE ra.release_id = r.id
                       ORDER BY ra.position
                       LIMIT 1
                    ),
                    (
                      SELECT a.name::text
                        FROM furumusic__track t
                        JOIN furumusic__track_artist ta ON ta.track_id = t.id
                        JOIN furumusic__artist a ON a.id = ta.artist_id
                       WHERE t.release_id = r.id AND ta.role <> 'featuring'
                       ORDER BY t.disc_number NULLS LAST, t.track_number NULLS LAST, ta.position
                       LIMIT 1
                    )
                  ) AS artist_name
             FROM furumusic__release r
             LEFT JOIN furumusic__artwork_lookup_state s
               ON s.entity_kind = 'release'
              AND s.entity_id = r.id
              AND s.source = 'lastfm'
            WHERE r.cover_file_id IS NULL
              AND r.is_hidden = false
              AND (
                    s.entity_id IS NULL
                 OR s.status = 'failed' AND s.last_attempt_at < $1
                 OR s.status = 'not_found' AND (s.attempt_count < 3 OR s.last_attempt_at < $2)
                 OR s.status = 'found' AND s.last_attempt_at < $1
              )
            ORDER BY s.last_attempt_at NULLS FIRST, r.id
            LIMIT $3"#,
    )
    .bind(&failed_cutoff)
    .bind(&not_found_cutoff)
    .bind(MAX_LASTFM_RELEASE_LOOKUPS)
    .fetch_all(&ctx.pool)
    .await?;

    if releases.is_empty() {
        log.info("Release Last.fm artwork pass: no eligible releases need lookup");
        return Ok(());
    }
    log.info(&format!(
        "Release Last.fm artwork pass: looking up {} release(s)",
        releases.len()
    ));

    for (index, release) in releases.iter().enumerate() {
        let Some(artist_name) = release
            .artist_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            stats.release_lastfm_not_found += 1;
            record_lookup_state(
                &ctx.pool,
                "release",
                release.id,
                "not_found",
                Some("release has no primary artist for Last.fm lookup"),
                None,
            )
            .await?;
            log.warn(&format!(
                "Release {} \"{}\": no primary artist for Last.fm lookup",
                release.id, release.title
            ));
            continue;
        };

        log.info(&format!(
            "Release Last.fm artwork {}/{}: release {} \"{}\" by \"{}\"",
            index + 1,
            releases.len(),
            release.id,
            release.title,
            artist_name
        ));

        match fetch_lastfm_album_image(client, api_key, artist_name, &release.title).await {
            Ok(Some(image_url)) => match download_remote_cover(client, &image_url).await {
                Ok(cover) => match cover_art::save_cover_to_storage(
                    &ctx.db,
                    &ctx.pool,
                    storage_dir,
                    artist_name,
                    &release.title,
                    &cover,
                )
                .await
                {
                    Ok(cover_file_id) => {
                        cover_art::assign_cover_to_release(&ctx.pool, release.id, cover_file_id)
                            .await?;
                        record_lookup_state(
                            &ctx.pool,
                            "release",
                            release.id,
                            "found",
                            None,
                            Some(&image_url),
                        )
                        .await?;
                        stats.release_lastfm_assigned += 1;
                        log.info(&format!(
                            "Release {} \"{}\": assigned Last.fm cover",
                            release.id, release.title
                        ));
                    }
                    Err(err) => {
                        stats.failed += 1;
                        record_lookup_state(
                            &ctx.pool,
                            "release",
                            release.id,
                            "failed",
                            Some(&err.to_string()),
                            Some(&image_url),
                        )
                        .await?;
                        log.warn(&format!(
                            "Release {} \"{}\": failed to save Last.fm cover: {err}",
                            release.id, release.title
                        ));
                    }
                },
                Err(err) => {
                    stats.failed += 1;
                    record_lookup_state(
                        &ctx.pool,
                        "release",
                        release.id,
                        "failed",
                        Some(&err.to_string()),
                        Some(&image_url),
                    )
                    .await?;
                    log.warn(&format!(
                        "Release {} \"{}\": failed to download Last.fm cover: {err}",
                        release.id, release.title
                    ));
                }
            },
            Ok(None) => {
                stats.release_lastfm_not_found += 1;
                record_lookup_state(&ctx.pool, "release", release.id, "not_found", None, None)
                    .await?;
                log.info(&format!(
                    "Release {} \"{}\": Last.fm did not return artwork",
                    release.id, release.title
                ));
            }
            Err(err) if err.to_string().contains("rate limit") => {
                stats.failed += 1;
                record_lookup_state(
                    &ctx.pool,
                    "release",
                    release.id,
                    "failed",
                    Some(&err.to_string()),
                    None,
                )
                .await?;
                log.error(
                    "Last.fm rate limit exceeded during release artwork lookup; stopping this pass",
                );
                break;
            }
            Err(err) => {
                stats.failed += 1;
                record_lookup_state(
                    &ctx.pool,
                    "release",
                    release.id,
                    "failed",
                    Some(&err.to_string()),
                    None,
                )
                .await?;
                log.warn(&format!(
                    "Release {} \"{}\": Last.fm artwork lookup failed: {err}",
                    release.id, release.title
                ));
            }
        }

        tokio::time::sleep(LASTFM_REQUEST_DELAY).await;
    }

    Ok(())
}

async fn backfill_artist_lastfm(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    api_key: &str,
    client: &Client,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let failed_cutoff = cutoff_iso(1);
    let not_found_cutoff = cutoff_iso(30);
    let artists = sqlx::query_as::<_, ArtistCandidate>(
        r#"SELECT a.id, a.name::text AS name
             FROM furumusic__artist a
             LEFT JOIN furumusic__media_file mf ON mf.id = a.image_file_id
             LEFT JOIN furumusic__artwork_lookup_state s
               ON s.entity_kind = 'artist'
              AND s.entity_id = a.id
              AND s.source = 'lastfm'
            WHERE (
                    a.image_file_id IS NULL
                 OR mf.file_path NOT LIKE '%/__artist_image__/%'
                  )
              AND a.is_hidden = false
              AND (
                    s.entity_id IS NULL
                 OR s.status = 'failed' AND s.last_attempt_at < $1
                 OR s.status = 'not_found' AND (s.attempt_count < 3 OR s.last_attempt_at < $2)
                 OR s.status = 'found' AND s.last_attempt_at < $1
              )
            ORDER BY s.last_attempt_at NULLS FIRST, a.id
            LIMIT $3"#,
    )
    .bind(&failed_cutoff)
    .bind(&not_found_cutoff)
    .bind(MAX_LASTFM_ARTIST_LOOKUPS)
    .fetch_all(&ctx.pool)
    .await?;

    if artists.is_empty() {
        log.info("Artist Last.fm artwork pass: no eligible artists need lookup");
        return Ok(());
    }
    log.info(&format!(
        "Artist Last.fm artwork pass: looking up {} artist(s)",
        artists.len()
    ));

    for (index, artist) in artists.iter().enumerate() {
        log.info(&format!(
            "Artist Last.fm artwork {}/{}: artist {} \"{}\"",
            index + 1,
            artists.len(),
            artist.id,
            artist.name
        ));

        match fetch_lastfm_artist_image(client, api_key, &artist.name).await {
            Ok(Some(image_url)) => match download_remote_cover(client, &image_url).await {
                Ok(cover) => match cover_art::save_cover_to_storage(
                    &ctx.db,
                    &ctx.pool,
                    storage_dir,
                    &artist.name,
                    "__artist_image__",
                    &cover,
                )
                .await
                {
                    Ok(image_file_id) => {
                        sqlx::query(
                            r#"UPDATE furumusic__artist
                                      SET image_file_id = $1,
                                          updated_at = $3
                                    WHERE id = $2
                                      AND (
                                            image_file_id IS NULL
                                         OR EXISTS (
                                                SELECT 1
                                                  FROM furumusic__media_file mf
                                                 WHERE mf.id = furumusic__artist.image_file_id
                                                   AND mf.file_path NOT LIKE '%/__artist_image__/%'
                                            )
                                          )"#,
                        )
                        .bind(image_file_id)
                        .bind(artist.id)
                        .bind(now_iso())
                        .execute(&ctx.pool)
                        .await?;
                        record_lookup_state(
                            &ctx.pool,
                            "artist",
                            artist.id,
                            "found",
                            None,
                            Some(&image_url),
                        )
                        .await?;
                        stats.artist_lastfm_assigned += 1;
                        log.info(&format!(
                            "Artist {} \"{}\": assigned Last.fm image",
                            artist.id, artist.name
                        ));
                    }
                    Err(err) => {
                        stats.failed += 1;
                        record_lookup_state(
                            &ctx.pool,
                            "artist",
                            artist.id,
                            "failed",
                            Some(&err.to_string()),
                            Some(&image_url),
                        )
                        .await?;
                        log.warn(&format!(
                            "Artist {} \"{}\": failed to save Last.fm image: {err}",
                            artist.id, artist.name
                        ));
                    }
                },
                Err(err) => {
                    stats.failed += 1;
                    record_lookup_state(
                        &ctx.pool,
                        "artist",
                        artist.id,
                        "failed",
                        Some(&err.to_string()),
                        Some(&image_url),
                    )
                    .await?;
                    log.warn(&format!(
                        "Artist {} \"{}\": failed to download Last.fm image: {err}",
                        artist.id, artist.name
                    ));
                }
            },
            Ok(None) => {
                record_lookup_state(&ctx.pool, "artist", artist.id, "not_found", None, None)
                    .await?;
                log.info(&format!(
                    "Artist {} \"{}\": Last.fm did not return artwork",
                    artist.id, artist.name
                ));
                stats.artist_lastfm_not_found += 1;
                match assign_artist_album_fallback(ctx, artist.id).await {
                    Ok(Some(media_file_id)) => {
                        stats.artist_album_fallback_assigned += 1;
                        log.info(&format!(
                            "Artist {} \"{}\": assigned random local album cover (media_file_id={media_file_id})",
                            artist.id, artist.name
                        ));
                    }
                    Ok(None) => {
                        log.info(&format!(
                            "Artist {} \"{}\": no local album cover available for fallback",
                            artist.id, artist.name
                        ));
                    }
                    Err(err) => {
                        stats.failed += 1;
                        log.warn(&format!(
                            "Artist {} \"{}\": failed to assign album fallback artwork: {err}",
                            artist.id, artist.name
                        ));
                    }
                }
            }
            Err(err) if err.to_string().contains("rate limit") => {
                stats.failed += 1;
                record_lookup_state(
                    &ctx.pool,
                    "artist",
                    artist.id,
                    "failed",
                    Some(&err.to_string()),
                    None,
                )
                .await?;
                log.error(
                    "Last.fm rate limit exceeded during artist artwork lookup; stopping this pass",
                );
                break;
            }
            Err(err) => {
                stats.failed += 1;
                record_lookup_state(
                    &ctx.pool,
                    "artist",
                    artist.id,
                    "failed",
                    Some(&err.to_string()),
                    None,
                )
                .await?;
                log.warn(&format!(
                    "Artist {} \"{}\": Last.fm artwork lookup failed: {err}",
                    artist.id, artist.name
                ));
            }
        }

        tokio::time::sleep(LASTFM_REQUEST_DELAY).await;
    }

    Ok(())
}

async fn backfill_artist_album_fallbacks(
    ctx: &JobContext,
    log: &mut JobLog,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let artists = sqlx::query_as::<_, ArtistCandidate>(
        r#"SELECT a.id, a.name::text AS name
             FROM furumusic__artist a
            WHERE a.image_file_id IS NULL
              AND a.is_hidden = false
              AND EXISTS (
                    SELECT 1
                      FROM furumusic__release_artist ra
                      JOIN furumusic__release r ON r.id = ra.release_id
                     WHERE ra.artist_id = a.id
                       AND r.cover_file_id IS NOT NULL
                       AND r.is_hidden = false
                    UNION
                    SELECT 1
                      FROM furumusic__track_artist ta
                      JOIN furumusic__track t ON t.id = ta.track_id
                      JOIN furumusic__release r ON r.id = t.release_id
                     WHERE ta.artist_id = a.id
                       AND r.cover_file_id IS NOT NULL
                       AND r.is_hidden = false
                  )
            ORDER BY a.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    if artists.is_empty() {
        log.info("Artist album fallback pass: no artists need local album fallback");
        return Ok(());
    }

    log.info(&format!(
        "Artist album fallback pass: checking {} artist(s) without images",
        artists.len()
    ));

    for artist in artists {
        match assign_artist_album_fallback(ctx, artist.id).await {
            Ok(Some(media_file_id)) => {
                stats.artist_album_fallback_assigned += 1;
                log.info(&format!(
                    "Artist {} \"{}\": assigned local album cover fallback (media_file_id={media_file_id})",
                    artist.id, artist.name
                ));
            }
            Ok(None) => {}
            Err(err) => {
                stats.failed += 1;
                log.warn(&format!(
                    "Artist {} \"{}\": failed to assign album fallback artwork: {err}",
                    artist.id, artist.name
                ));
            }
        }
    }

    Ok(())
}

async fn repair_cover_variants(
    ctx: &JobContext,
    log: &mut JobLog,
    storage_dir: &str,
    stats: &mut ArtworkStats,
) -> anyhow::Result<()> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        r#"SELECT DISTINCT mf.id, mf.file_path::text
             FROM furumusic__media_file mf
            WHERE mf.file_type = 'cover_art'
              AND (
                    EXISTS (SELECT 1 FROM furumusic__release r WHERE r.cover_file_id = mf.id)
                 OR EXISTS (SELECT 1 FROM furumusic__track t WHERE t.cover_file_id = mf.id)
                 OR EXISTS (SELECT 1 FROM furumusic__artist a WHERE a.image_file_id = mf.id)
                  )
            ORDER BY mf.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    if rows.is_empty() {
        log.info("Cover variant pass: no referenced cover art media files found");
        return Ok(());
    }
    log.info(&format!(
        "Cover variant pass: checking {} referenced cover art media file(s)",
        rows.len()
    ));

    for (media_file_id, file_path) in rows {
        let path = crate::media_paths::resolve_media_file_path(storage_dir, &file_path);
        if !path.exists() {
            stats.variants_missing_original += 1;
            log.warn(&format!(
                "Media file {media_file_id}: original cover not found at {}",
                path.display()
            ));
            continue;
        }

        match cover_variants::ensure_cover_variants(&path).await {
            Ok(0) => stats.variants_unchanged += 1,
            Ok(count) => {
                stats.variants_created += count;
                log.info(&format!(
                    "Media file {media_file_id}: created {count} variant(s)"
                ));
            }
            Err(err) => {
                stats.failed += 1;
                log.warn(&format!(
                    "Media file {media_file_id}: failed to create variants: {err}"
                ));
            }
        }
    }

    Ok(())
}

async fn assign_artist_album_fallback(
    ctx: &JobContext,
    artist_id: i64,
) -> anyhow::Result<Option<i64>> {
    let media_file_id: Option<i64> = sqlx::query_scalar(
        r#"SELECT media_file_id
             FROM (
                    SELECT DISTINCT r.cover_file_id AS media_file_id
                      FROM furumusic__release r
                      JOIN furumusic__release_artist ra ON ra.release_id = r.id
                     WHERE ra.artist_id = $1
                       AND r.cover_file_id IS NOT NULL
                       AND r.is_hidden = false
                    UNION
                    SELECT DISTINCT r.cover_file_id AS media_file_id
                      FROM furumusic__release r
                      JOIN furumusic__track t ON t.release_id = r.id
                      JOIN furumusic__track_artist ta ON ta.track_id = t.id
                     WHERE ta.artist_id = $1
                       AND r.cover_file_id IS NOT NULL
                       AND r.is_hidden = false
                  ) covers
            ORDER BY random()
            LIMIT 1"#,
    )
    .bind(artist_id)
    .fetch_optional(&ctx.pool)
    .await?;

    let Some(media_file_id) = media_file_id else {
        return Ok(None);
    };

    let result = sqlx::query(
        r#"UPDATE furumusic__artist
              SET image_file_id = $1,
                  updated_at = $3
            WHERE id = $2
              AND image_file_id IS NULL"#,
    )
    .bind(media_file_id)
    .bind(artist_id)
    .bind(now_iso())
    .execute(&ctx.pool)
    .await?;

    if result.rows_affected() == 0 {
        Ok(None)
    } else {
        Ok(Some(media_file_id))
    }
}

async fn fetch_lastfm_album_image(
    client: &Client,
    api_key: &str,
    artist: &str,
    album: &str,
) -> anyhow::Result<Option<String>> {
    let response = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[
            ("method", "album.getInfo"),
            ("api_key", api_key),
            ("artist", artist),
            ("album", album),
            ("autocorrect", "1"),
            ("format", "json"),
        ])
        .send()
        .await?;
    let body = response.text().await?;
    let parsed: LastfmAlbumResponse = serde_json::from_str(&body)?;
    if let Some(code) = parsed.error {
        if code == 6 || code == 7 {
            return Ok(None);
        }
        if code == 29 {
            anyhow::bail!("Last.fm rate limit exceeded");
        }
        anyhow::bail!(
            "Last.fm API error {code}: {}",
            parsed.message.unwrap_or_default()
        );
    }
    Ok(parsed
        .album
        .and_then(|album| choose_best_image(album.image)))
}

async fn fetch_lastfm_artist_image(
    client: &Client,
    api_key: &str,
    artist: &str,
) -> anyhow::Result<Option<String>> {
    if let Some(image_url) = fetch_lastfm_artist_info_image(client, api_key, artist).await? {
        return Ok(Some(image_url));
    }

    fetch_lastfm_artist_top_album_image(client, api_key, artist).await
}

async fn fetch_lastfm_artist_info_image(
    client: &Client,
    api_key: &str,
    artist: &str,
) -> anyhow::Result<Option<String>> {
    let response = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[
            ("method", "artist.getInfo"),
            ("api_key", api_key),
            ("artist", artist),
            ("autocorrect", "1"),
            ("format", "json"),
        ])
        .send()
        .await?;
    let body = response.text().await?;
    let parsed: LastfmArtistResponse = serde_json::from_str(&body)?;
    if let Some(code) = parsed.error {
        if code == 6 || code == 7 {
            return Ok(None);
        }
        if code == 29 {
            anyhow::bail!("Last.fm rate limit exceeded");
        }
        anyhow::bail!(
            "Last.fm API error {code}: {}",
            parsed.message.unwrap_or_default()
        );
    }
    Ok(parsed
        .artist
        .and_then(|artist| choose_best_image(artist.image)))
}

async fn fetch_lastfm_artist_top_album_image(
    client: &Client,
    api_key: &str,
    artist: &str,
) -> anyhow::Result<Option<String>> {
    let response = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[
            ("method", "artist.getTopAlbums"),
            ("api_key", api_key),
            ("artist", artist),
            ("autocorrect", "1"),
            ("limit", "10"),
            ("format", "json"),
        ])
        .send()
        .await?;
    let body = response.text().await?;
    let parsed: LastfmTopAlbumsResponse = serde_json::from_str(&body)?;
    if let Some(code) = parsed.error {
        if code == 6 || code == 7 {
            return Ok(None);
        }
        if code == 29 {
            anyhow::bail!("Last.fm rate limit exceeded");
        }
        anyhow::bail!(
            "Last.fm API error {code}: {}",
            parsed.message.unwrap_or_default()
        );
    }

    let albums = parsed
        .topalbums
        .map(|topalbums| topalbums.album)
        .unwrap_or_default();
    Ok(albums
        .into_iter()
        .filter_map(|album| choose_best_image(album.image))
        .next())
}

fn deserialize_one_or_many<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: de::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(Vec::new());
    };

    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(de::Error::custom))
            .collect(),
        serde_json::Value::Object(_) => serde_json::from_value(value)
            .map(|item| vec![item])
            .map_err(de::Error::custom),
        _ => Ok(Vec::new()),
    }
}

fn choose_best_image(images: Option<Vec<LastfmImage>>) -> Option<String> {
    let mut images = images.unwrap_or_default();
    images.sort_by_key(|image| image_size_rank(&image.size));
    images
        .into_iter()
        .rev()
        .map(|image| image.url.trim().to_string())
        .find(|url| is_usable_lastfm_image(url))
}

fn image_size_rank(size: &str) -> u8 {
    match size {
        "mega" => 5,
        "extralarge" => 4,
        "large" => 3,
        "medium" => 2,
        "small" => 1,
        _ => 0,
    }
}

fn is_usable_lastfm_image(url: &str) -> bool {
    let value = url.trim();
    !value.is_empty()
        && !value.contains("2a96cbd8b46e442fc41c2b86b821562f")
        && !value.contains("default_")
}

async fn download_remote_cover(client: &Client, url: &str) -> anyhow::Result<CoverImage> {
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("image download failed with HTTP {}", response.status());
    }
    let header_mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(normalize_image_mime);
    let data = response.bytes().await?.to_vec();
    if data.is_empty() {
        anyhow::bail!("downloaded image is empty");
    }
    let mime_type = header_mime
        .or_else(|| guess_image_mime(&data))
        .ok_or_else(|| anyhow::anyhow!("downloaded file is not a supported image"))?;
    Ok(CoverImage {
        data,
        mime_type,
        source: CoverSource::Remote(url.to_string()),
    })
}

fn normalize_image_mime(value: &str) -> Option<String> {
    let mime = value.split(';').next()?.trim().to_ascii_lowercase();
    match mime.as_str() {
        "image/jpeg" | "image/jpg" => Some("image/jpeg".to_string()),
        "image/png" => Some("image/png".to_string()),
        "image/webp" => Some("image/webp".to_string()),
        "image/gif" => Some("image/gif".to_string()),
        "image/bmp" => Some("image/bmp".to_string()),
        _ => None,
    }
}

fn guess_image_mime(data: &[u8]) -> Option<String> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg".to_string())
    } else if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("image/png".to_string())
    } else if data.starts_with(b"RIFF") && data.len() > 12 && &data[8..12] == b"WEBP" {
        Some("image/webp".to_string())
    } else if data.starts_with(b"GIF8") {
        Some("image/gif".to_string())
    } else if data.starts_with(&[0x42, 0x4D]) {
        Some("image/bmp".to_string())
    } else {
        None
    }
}

async fn record_lookup_state(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
    status: &str,
    error: Option<&str>,
    source_url: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO furumusic__artwork_lookup_state
              (entity_kind, entity_id, source, status, attempt_count, last_attempt_at, last_error, source_url)
           VALUES ($1, $2, 'lastfm', $3, 1, $4, $5, $6)
           ON CONFLICT (entity_kind, entity_id, source) DO UPDATE SET
              status = EXCLUDED.status,
              attempt_count = furumusic__artwork_lookup_state.attempt_count + 1,
              last_attempt_at = EXCLUDED.last_attempt_at,
              last_error = EXCLUDED.last_error,
              source_url = EXCLUDED.source_url"#,
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(status)
    .bind(now_iso())
    .bind(error)
    .bind(source_url)
    .execute(pool)
    .await?;
    Ok(())
}

async fn reset_lookup_state(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"DELETE FROM furumusic__artwork_lookup_state
            WHERE entity_kind = $1
              AND entity_id = $2
              AND source = 'lastfm'"#,
    )
    .bind(entity_kind)
    .bind(entity_id)
    .execute(pool)
    .await?;
    Ok(())
}

fn cover_source_description(source: &CoverSource) -> String {
    match source {
        CoverSource::FolderFile(path) => format!("folder: {}", path.display()),
        CoverSource::Embedded(path) => format!("embedded: {}", path.display()),
        CoverSource::Remote(url) => format!("remote: {url}"),
    }
}

fn cutoff_iso(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
