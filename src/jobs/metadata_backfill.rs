use std::time::Duration;

use serde_json::Value;

use crate::scheduler::{Job, JobContext, JobLog};

const LASTFM_TAG_REQUEST_DELAY: Duration = Duration::from_millis(1200);
const LASTFM_TAG_LIMIT: usize = 12;

#[derive(Debug, Clone, Copy)]
pub struct MetadataBackfillOptions {
    pub audio_bitrate: bool,
    pub audio_sample_rate: bool,
    pub audio_bit_depth: bool,
    pub duration_seconds: bool,
    pub local_genres: bool,
    pub lastfm_tags: bool,
    pub overwrite: bool,
}

impl MetadataBackfillOptions {
    pub fn any_field(self) -> bool {
        self.audio_bitrate
            || self.audio_sample_rate
            || self.audio_bit_depth
            || self.duration_seconds
            || self.local_genres
            || self.lastfm_tags
    }

    fn needs_file_scan(self) -> bool {
        self.audio_bitrate
            || self.audio_sample_rate
            || self.audio_bit_depth
            || self.duration_seconds
            || self.local_genres
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

#[derive(sqlx::FromRow)]
struct LastfmArtistTagRow {
    id: i64,
    name: String,
}

#[derive(sqlx::FromRow)]
struct LastfmReleaseTagRow {
    id: i64,
    title: String,
    artist_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct LastfmTrackTagRow {
    id: i64,
    title: String,
    artist_name: Option<String>,
}

#[derive(Debug, Clone)]
struct TagCandidate {
    name: String,
    weight: f64,
}

#[derive(Debug, Default)]
struct LastfmTagStats {
    considered: u64,
    updated_entities: u64,
    tags_saved: u64,
    skipped_existing: u64,
    not_found: u64,
    failed: u64,
}

pub struct MetadataBackfillJob;

#[async_trait::async_trait]
impl Job for MetadataBackfillJob {
    fn name(&self) -> &'static str {
        "metadata_backfill"
    }

    fn description(&self) -> &'static str {
        "Backfill technical audio metadata, local genres, and Last.fm tags"
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
                local_genres: true,
                lastfm_tags: true,
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

    let mut scanned = 0u64;
    let mut media_updated = 0u64;
    let mut track_updated = 0u64;
    let mut local_tags_updated = 0u64;
    let mut unchanged = 0u64;
    let mut missing = 0u64;
    let mut failed = 0u64;

    log.info(&format!(
        "Metadata backfill options: file_scan={}, local_genres={}, lastfm_tags={}, mode={}",
        options.needs_file_scan(),
        options.local_genres,
        options.lastfm_tags,
        if options.overwrite {
            "overwrite"
        } else {
            "fill_missing"
        }
    ));

    if options.needs_file_scan() {
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
            "Metadata file backfill started: {} audio file(s), mode={}",
            rows.len(),
            if options.overwrite {
                "overwrite"
            } else {
                "fill_missing"
            }
        ));

        for row in rows {
            scanned += 1;
            let path = crate::media_paths::resolve_media_file_path(
                &ctx.config.agent_storage_dir,
                &row.file_path,
            );
            if !path.exists() {
                missing += 1;
                log.warn(&format!("missing file: {}", row.file_path));
                continue;
            }

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
            if options.audio_sample_rate && should_update(row.audio_sample_rate, options.overwrite)
            {
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

            let mut changed_tags = false;
            if options.local_genres {
                if let (Some(track_id), Some(genre)) = (row.track_id, raw_meta.genre.as_deref()) {
                    let saved = save_track_tag_text(
                        &ctx.pool,
                        track_id,
                        genre,
                        "file",
                        options.overwrite,
                    )
                    .await?;
                    if saved > 0 {
                        local_tags_updated += saved;
                        changed_tags = true;
                    }
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

            if !changed_media && !changed_track && !changed_tags {
                unchanged += 1;
            }

            if scanned % 100 == 0 {
                log.info(&format!(
                    "Progress: {scanned} scanned, {media_updated} media updated, {track_updated} tracks updated, {local_tags_updated} local tags saved, {unchanged} unchanged, {missing} missing, {failed} failed"
                ));
            }
        }
    }

    let lastfm_stats = if options.lastfm_tags {
        log.info("Metadata file backfill finished; starting Last.fm tag backfill");
        backfill_lastfm_tags(ctx, log, options.overwrite).await?
    } else {
        log.info("Last.fm tag backfill disabled for this run");
        LastfmTagStats::default()
    };

    log.info(&format!(
        "Metadata backfill complete: {scanned} scanned, {media_updated} media updated, {track_updated} tracks updated, {local_tags_updated} local tags saved, {unchanged} unchanged, {missing} missing, {failed} failed; Last.fm tags: considered={}, updated_entities={}, tags_saved={}, skipped_existing={}, not_found={}, failed={}",
        lastfm_stats.considered,
        lastfm_stats.updated_entities,
        lastfm_stats.tags_saved,
        lastfm_stats.skipped_existing,
        lastfm_stats.not_found,
        lastfm_stats.failed,
    ));
    Ok(())
}

pub async fn save_approved_track_genres(
    pool: &sqlx::PgPool,
    track_id: i64,
    genre_text: &str,
) -> anyhow::Result<u64> {
    save_track_tag_text(pool, track_id, genre_text, "review", false).await
}

async fn backfill_lastfm_tags(
    ctx: &JobContext,
    log: &mut JobLog,
    overwrite: bool,
) -> anyhow::Result<LastfmTagStats> {
    let api_key = ctx.config.lastfm_api_key.trim();
    if api_key.is_empty() {
        log.warn("lastfm_api_key is not configured, skipping Last.fm tag backfill");
        return Ok(LastfmTagStats::default());
    }

    log.info("Last.fm tag backfill started");

    let client = reqwest::Client::builder()
        .user_agent("furumusic-metadata-backfill/0.1")
        .timeout(Duration::from_secs(15))
        .build()?;

    let mut stats = LastfmTagStats::default();
    backfill_lastfm_artist_tags(ctx, log, &client, api_key, overwrite, &mut stats).await?;
    backfill_lastfm_release_tags(ctx, log, &client, api_key, overwrite, &mut stats).await?;
    backfill_lastfm_track_tags(ctx, log, &client, api_key, overwrite, &mut stats).await?;
    Ok(stats)
}

async fn backfill_lastfm_artist_tags(
    ctx: &JobContext,
    log: &mut JobLog,
    client: &reqwest::Client,
    api_key: &str,
    overwrite: bool,
    stats: &mut LastfmTagStats,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, LastfmArtistTagRow>(
        r#"SELECT DISTINCT a.id, a.name::text AS name
           FROM furumusic__artist a
           JOIN furumusic__track_artist ta ON ta.artist_id = a.id
           JOIN furumusic__track t ON t.id = ta.track_id
           WHERE a.is_hidden = false AND t.is_hidden = false
           ORDER BY a.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    log.info(&format!(
        "Last.fm artist tag pass: checking {} artist(s)",
        rows.len()
    ));
    let total = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        if should_skip_lastfm_entity(&ctx.pool, "artist", row.id, overwrite).await? {
            stats.skipped_existing += 1;
            if should_log_lastfm_progress(index + 1, total, 25) {
                log.info(&format!(
                    "Last.fm artist tags progress: {}/{}",
                    index + 1,
                    total
                ));
            }
            continue;
        }
        stats.considered += 1;
        match fetch_lastfm_artist_tags(client, api_key, &row.name).await {
            Ok(tags) if !tags.is_empty() => {
                let saved =
                    replace_entity_tags(&ctx.pool, "artist", row.id, &tags, "lastfm", false)
                        .await?;
                stats.tags_saved += saved;
                stats.updated_entities += 1;
            }
            Ok(_) => {
                stats.not_found += 1;
            }
            Err(err) if err.to_string().contains("Last.fm rate limit exceeded") => {
                return Err(err);
            }
            Err(err) => {
                stats.failed += 1;
                log.warn(&format!(
                    "Last.fm artist tags failed for artist {} \"{}\": {err}",
                    row.id, row.name
                ));
            }
        }
        if should_log_lastfm_progress(index + 1, total, 25) {
            log.info(&format!(
                "Last.fm artist tags progress: {}/{}",
                index + 1,
                total
            ));
        }
        tokio::time::sleep(LASTFM_TAG_REQUEST_DELAY).await;
    }
    Ok(())
}

async fn backfill_lastfm_release_tags(
    ctx: &JobContext,
    log: &mut JobLog,
    client: &reqwest::Client,
    api_key: &str,
    overwrite: bool,
    stats: &mut LastfmTagStats,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, LastfmReleaseTagRow>(
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
           WHERE r.is_hidden = false
           ORDER BY r.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    log.info(&format!(
        "Last.fm release tag pass: checking {} release(s)",
        rows.len()
    ));
    let total = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        if should_skip_lastfm_entity(&ctx.pool, "release", row.id, overwrite).await? {
            stats.skipped_existing += 1;
            if should_log_lastfm_progress(index + 1, total, 25) {
                log.info(&format!(
                    "Last.fm release tags progress: {}/{}",
                    index + 1,
                    total
                ));
            }
            continue;
        }
        let Some(artist) = row.artist_name.as_deref().filter(|value| !value.trim().is_empty())
        else {
            stats.not_found += 1;
            if should_log_lastfm_progress(index + 1, total, 25) {
                log.info(&format!(
                    "Last.fm release tags progress: {}/{}",
                    index + 1,
                    total
                ));
            }
            continue;
        };
        stats.considered += 1;
        match fetch_lastfm_album_tags(client, api_key, artist, &row.title).await {
            Ok(tags) if !tags.is_empty() => {
                let saved =
                    replace_entity_tags(&ctx.pool, "release", row.id, &tags, "lastfm", false)
                        .await?;
                stats.tags_saved += saved;
                stats.updated_entities += 1;
            }
            Ok(_) => {
                stats.not_found += 1;
            }
            Err(err) if err.to_string().contains("Last.fm rate limit exceeded") => {
                return Err(err);
            }
            Err(err) => {
                stats.failed += 1;
                log.warn(&format!(
                    "Last.fm release tags failed for release {} \"{}\" / \"{}\": {err}",
                    row.id, artist, row.title
                ));
            }
        }
        if should_log_lastfm_progress(index + 1, total, 25) {
            log.info(&format!(
                "Last.fm release tags progress: {}/{}",
                index + 1,
                total
            ));
        }
        tokio::time::sleep(LASTFM_TAG_REQUEST_DELAY).await;
    }
    Ok(())
}

async fn backfill_lastfm_track_tags(
    ctx: &JobContext,
    log: &mut JobLog,
    client: &reqwest::Client,
    api_key: &str,
    overwrite: bool,
    stats: &mut LastfmTagStats,
) -> anyhow::Result<()> {
    let rows = sqlx::query_as::<_, LastfmTrackTagRow>(
        r#"SELECT t.id,
                  t.title::text AS title,
                  (
                    SELECT a.name::text
                      FROM furumusic__track_artist ta
                      JOIN furumusic__artist a ON a.id = ta.artist_id
                     WHERE ta.track_id = t.id AND ta.role <> 'featuring'
                     ORDER BY ta.position
                     LIMIT 1
                  ) AS artist_name
           FROM furumusic__track t
           WHERE t.is_hidden = false
           ORDER BY t.id"#,
    )
    .fetch_all(&ctx.pool)
    .await?;

    log.info(&format!(
        "Last.fm track tag pass: checking {} track(s)",
        rows.len()
    ));
    let total = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        if should_skip_lastfm_entity(&ctx.pool, "track", row.id, overwrite).await? {
            stats.skipped_existing += 1;
            if should_log_lastfm_progress(index + 1, total, 50) {
                log.info(&format!(
                    "Last.fm track tags progress: {}/{}",
                    index + 1,
                    total
                ));
            }
            continue;
        }
        let Some(artist) = row.artist_name.as_deref().filter(|value| !value.trim().is_empty())
        else {
            stats.not_found += 1;
            if should_log_lastfm_progress(index + 1, total, 50) {
                log.info(&format!(
                    "Last.fm track tags progress: {}/{}",
                    index + 1,
                    total
                ));
            }
            continue;
        };
        stats.considered += 1;
        match fetch_lastfm_track_tags(client, api_key, artist, &row.title).await {
            Ok(tags) if !tags.is_empty() => {
                let saved =
                    replace_entity_tags(&ctx.pool, "track", row.id, &tags, "lastfm", true).await?;
                stats.tags_saved += saved;
                stats.updated_entities += 1;
            }
            Ok(_) => {
                stats.not_found += 1;
            }
            Err(err) if err.to_string().contains("Last.fm rate limit exceeded") => {
                return Err(err);
            }
            Err(err) => {
                stats.failed += 1;
                log.warn(&format!(
                    "Last.fm track tags failed for track {} \"{}\" / \"{}\": {err}",
                    row.id, artist, row.title
                ));
            }
        }
        if should_log_lastfm_progress(index + 1, total, 50) {
            log.info(&format!(
                "Last.fm track tags progress: {}/{}",
                index + 1,
                total
            ));
        }
        tokio::time::sleep(LASTFM_TAG_REQUEST_DELAY).await;
    }
    Ok(())
}

fn should_log_lastfm_progress(done: usize, total: usize, every: usize) -> bool {
    total > 0 && (done == total || done % every == 0)
}

async fn should_skip_lastfm_entity(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
    overwrite: bool,
) -> anyhow::Result<bool> {
    if overwrite {
        return Ok(false);
    }
    let exists: Option<i64> = sqlx::query_scalar(
        r#"SELECT 1
           FROM furumusic__entity_genre_tag
           WHERE entity_kind = $1 AND entity_id = $2 AND source = 'lastfm'
           LIMIT 1"#,
    )
    .bind(entity_kind)
    .bind(entity_id)
    .fetch_optional(pool)
    .await?;
    Ok(exists.is_some())
}

async fn fetch_lastfm_artist_tags(
    client: &reqwest::Client,
    api_key: &str,
    artist: &str,
) -> anyhow::Result<Vec<TagCandidate>> {
    fetch_lastfm_top_tags(
        client,
        &[
            ("method", "artist.getTopTags"),
            ("api_key", api_key),
            ("artist", artist),
            ("autocorrect", "1"),
            ("format", "json"),
        ],
    )
    .await
}

async fn fetch_lastfm_album_tags(
    client: &reqwest::Client,
    api_key: &str,
    artist: &str,
    album: &str,
) -> anyhow::Result<Vec<TagCandidate>> {
    fetch_lastfm_top_tags(
        client,
        &[
            ("method", "album.getTopTags"),
            ("api_key", api_key),
            ("artist", artist),
            ("album", album),
            ("autocorrect", "1"),
            ("format", "json"),
        ],
    )
    .await
}

async fn fetch_lastfm_track_tags(
    client: &reqwest::Client,
    api_key: &str,
    artist: &str,
    track: &str,
) -> anyhow::Result<Vec<TagCandidate>> {
    fetch_lastfm_top_tags(
        client,
        &[
            ("method", "track.getTopTags"),
            ("api_key", api_key),
            ("artist", artist),
            ("track", track),
            ("autocorrect", "1"),
            ("format", "json"),
        ],
    )
    .await
}

async fn fetch_lastfm_top_tags(
    client: &reqwest::Client,
    query: &[(&str, &str)],
) -> anyhow::Result<Vec<TagCandidate>> {
    let response = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(query)
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let response = response.error_for_status()?;
    let body: Value = response.json().await?;
    if let Some(code) = body.get("error").and_then(|value| value.as_i64()) {
        if code == 29 {
            anyhow::bail!("Last.fm rate limit exceeded");
        }
        if code == 6 || code == 7 {
            return Ok(Vec::new());
        }
        anyhow::bail!(
            "Last.fm API error {code}: {}",
            body.get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error")
        );
    }

    let Some(tag_value) = body.get("toptags").and_then(|value| value.get("tag")) else {
        return Ok(Vec::new());
    };
    let mut tags = match tag_value {
        Value::Array(values) => values.iter().filter_map(tag_from_value).collect::<Vec<_>>(),
        Value::Object(_) => tag_from_value(tag_value).into_iter().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    tags.sort_by(|a, b| b.weight.total_cmp(&a.weight).then_with(|| a.name.cmp(&b.name)));
    tags.truncate(LASTFM_TAG_LIMIT);
    Ok(tags)
}

fn tag_from_value(value: &Value) -> Option<TagCandidate> {
    let name = value.get("name")?.as_str()?.trim();
    let name = clean_tag_name(name)?;
    let weight = value
        .get("count")
        .and_then(lastfm_count_to_f64)
        .unwrap_or(1.0)
        .max(1.0);
    Some(TagCandidate { name, weight })
}

fn lastfm_count_to_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
}

async fn save_track_tag_text(
    pool: &sqlx::PgPool,
    track_id: i64,
    tag_text: &str,
    source: &str,
    replace_source: bool,
) -> anyhow::Result<u64> {
    let tags = tags_from_text(tag_text);
    save_entity_tags(pool, "track", track_id, &tags, source, replace_source, true).await
}

async fn replace_entity_tags(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
    tags: &[TagCandidate],
    source: &str,
    mirror_track_genre: bool,
) -> anyhow::Result<u64> {
    save_entity_tags(
        pool,
        entity_kind,
        entity_id,
        tags,
        source,
        true,
        mirror_track_genre,
    )
    .await
}

async fn save_entity_tags(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
    tags: &[TagCandidate],
    source: &str,
    replace_source: bool,
    mirror_track_genre: bool,
) -> anyhow::Result<u64> {
    if tags.is_empty() {
        return Ok(0);
    }
    if replace_source {
        sqlx::query(
            r#"DELETE FROM furumusic__entity_genre_tag
               WHERE entity_kind = $1 AND entity_id = $2 AND source = $3"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .bind(source)
        .execute(pool)
        .await?;

        if mirror_track_genre && entity_kind == "track" {
            sqlx::query(
                r#"DELETE FROM furumusic__track_genre tg
                   WHERE tg.track_id = $1
                     AND NOT EXISTS (
                         SELECT 1
                           FROM furumusic__entity_genre_tag egt
                          WHERE egt.entity_kind = 'track'
                            AND egt.entity_id = tg.track_id
                            AND egt.genre_id = tg.genre_id
                     )"#,
            )
            .bind(entity_id)
            .execute(pool)
            .await?;
        }
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut saved = 0u64;
    for tag in tags {
        let Some(genre_id) = ensure_genre(pool, &tag.name).await? else {
            continue;
        };
        let result = sqlx::query(
            r#"INSERT INTO furumusic__entity_genre_tag
                  (entity_kind, entity_id, genre_id, source, weight, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6)
               ON CONFLICT (entity_kind, entity_id, genre_id, source) DO NOTHING"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .bind(genre_id)
        .bind(source)
        .bind(tag.weight)
        .bind(&now)
        .execute(pool)
        .await?;
        saved += result.rows_affected();

        if mirror_track_genre && entity_kind == "track" {
            let result = sqlx::query(
                r#"INSERT INTO furumusic__track_genre (track_id, genre_id)
                   VALUES ($1, $2)
                   ON CONFLICT (track_id, genre_id) DO NOTHING"#,
            )
            .bind(entity_id)
            .bind(genre_id)
            .execute(pool)
            .await?;
            saved += result.rows_affected();
        }
    }
    Ok(saved)
}

async fn ensure_genre(pool: &sqlx::PgPool, name: &str) -> anyhow::Result<Option<i64>> {
    let Some(name) = clean_tag_name(name) else {
        return Ok(None);
    };
    let normalized = normalize_tag_name(&name);
    if normalized.is_empty() || is_ignored_tag(&normalized) {
        return Ok(None);
    }

    let existing: Option<i64> = sqlx::query_scalar(
        r#"SELECT id FROM furumusic__genre
           WHERE name_normalized = $1
           ORDER BY id
           LIMIT 1"#,
    )
    .bind(&normalized)
    .fetch_optional(pool)
    .await?;
    if existing.is_some() {
        return Ok(existing);
    }

    let id = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO furumusic__genre (name, name_normalized)
           VALUES ($1, $2)
           ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
           RETURNING id"#,
    )
    .bind(&name)
    .bind(&normalized)
    .fetch_one(pool)
    .await?;
    Ok(Some(id))
}

fn tags_from_text(value: &str) -> Vec<TagCandidate> {
    let normalized_separators = value.replace(" / ", ";").replace('|', ";");
    let mut tags = Vec::new();
    for raw in normalized_separators.split([';', ',']) {
        if let Some(name) = clean_tag_name(raw) {
            if !is_ignored_tag(&normalize_tag_name(&name))
                && !tags
                    .iter()
                    .any(|tag: &TagCandidate| normalize_tag_name(&tag.name) == normalize_tag_name(&name))
            {
                tags.push(TagCandidate { name, weight: 1.0 });
            }
        }
    }
    tags
}

fn clean_tag_name(value: &str) -> Option<String> {
    let cleaned = value.trim().trim_matches('"').trim_matches('\'').trim();
    if cleaned.is_empty() {
        return None;
    }
    let cleaned = cleaned.chars().take(100).collect::<String>();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn normalize_tag_name(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_ignored_tag(normalized: &str) -> bool {
    matches!(
        normalized,
        "" | "unknown" | "undefined" | "none" | "n/a" | "na" | "other" | "misc" | "various"
    )
}

fn should_update<T>(current: Option<T>, overwrite: bool) -> bool {
    overwrite || current.is_none()
}

fn should_update_duration(current: Option<f64>, overwrite: bool) -> bool {
    overwrite || current.unwrap_or(0.0) <= 0.0
}
