use serde::Deserialize;

use crate::scheduler::{Job, JobContext, JobLog};

pub struct LastfmPopularityJob;

const LASTFM_REQUEST_DELAY: std::time::Duration = std::time::Duration::from_millis(1200);

#[derive(Debug, sqlx::FromRow)]
struct TrackLookupRow {
    id: i64,
    title: String,
    artist_name: Option<String>,
    lastfm_updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmTrackInfoResponse {
    track: Option<LastfmTrack>,
    error: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmTrack {
    listeners: Option<String>,
    playcount: Option<String>,
}

#[async_trait::async_trait]
impl Job for LastfmPopularityJob {
    fn name(&self) -> &'static str {
        "lastfm_popularity"
    }

    fn description(&self) -> &'static str {
        "Update Last.fm playcount/listener popularity for library tracks"
    }

    fn default_cron(&self) -> &'static str {
        // Sundays at 04:15
        "0 15 4 * * Sun"
    }

    async fn run(&self, ctx: &JobContext, log: &mut JobLog) -> anyhow::Result<()> {
        let api_key = ctx.config.lastfm_api_key.trim();
        if api_key.is_empty() {
            log.warn("lastfm_api_key is not configured, skipping Last.fm popularity update");
            return Ok(());
        }

        let tracks = sqlx::query_as::<_, TrackLookupRow>(
            r#"SELECT t.id,
                      t.title::text AS title,
                      t.lastfm_updated_at::text AS lastfm_updated_at,
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
                ORDER BY t.lastfm_updated_at IS NOT NULL, t.lastfm_updated_at ASC, t.id ASC"#,
        )
        .fetch_all(&ctx.pool)
        .await?;

        if tracks.is_empty() {
            log.info("No visible tracks found for Last.fm popularity update");
            return Ok(());
        }

        log.info(&format!(
            "Starting Last.fm popularity update for {} visible tracks; oldest or missing ratings are processed first; request delay is {} ms; rating formula is ln(playcount + 1) * ln(listeners + 1)",
            tracks.len(),
            LASTFM_REQUEST_DELAY.as_millis()
        ));

        let client = reqwest::Client::builder()
            .user_agent("furumusic-lastfm-popularity/0.1")
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        let mut updated = 0u64;
        let mut skipped = 0u64;
        let mut failed = 0u64;

        for (index, track) in tracks.iter().enumerate() {
            let Some(artist) = track
                .artist_name
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            else {
                skipped += 1;
                log.warn(&format!(
                    "Skipping track {} \"{}\": no primary artist",
                    track.id, track.title
                ));
                continue;
            };

            log.info(&format!(
                "Last.fm lookup {}/{}: track {} \"{}\" by \"{}\" (previous update: {})",
                index + 1,
                tracks.len(),
                track.id,
                track.title,
                artist,
                track.lastfm_updated_at.as_deref().unwrap_or("never")
            ));
            let result = fetch_track_info(&client, api_key, artist, &track.title).await;
            match result {
                Ok(Some((listeners, playcount))) => {
                    let rating = popularity_rating(listeners, playcount);
                    let fetched_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    sqlx::query(
                        r#"UPDATE furumusic__track
                              SET lastfm_listeners = $2,
                                  lastfm_playcount = $3,
                                  lastfm_rating = $4,
                                  lastfm_updated_at = $5
                            WHERE id = $1"#,
                    )
                    .bind(track.id)
                    .bind(listeners)
                    .bind(playcount)
                    .bind(rating)
                    .bind(&fetched_at)
                    .execute(&ctx.pool)
                    .await?;
                    sqlx::query(
                        r#"INSERT INTO furumusic__track_popularity_history
                              (track_id, source, listeners, playcount, rating, fetched_at)
                           VALUES ($1, 'lastfm', $2, $3, $4, $5)"#,
                    )
                    .bind(track.id)
                    .bind(listeners)
                    .bind(playcount)
                    .bind(rating)
                    .bind(&fetched_at)
                    .execute(&ctx.pool)
                    .await?;
                    updated += 1;
                    log.info(&format!(
                        "Updated track {} \"{}\" by \"{}\": listeners={listeners}, playcount={playcount}, rating={rating:.4}",
                        track.id, track.title, artist
                    ));
                }
                Ok(None) => {
                    skipped += 1;
                    log.warn(&format!(
                        "Last.fm has no usable match for track {} \"{}\" by \"{}\"",
                        track.id, track.title, artist
                    ));
                }
                Err(err) if err.to_string().contains("Last.fm rate limit exceeded") => {
                    failed += 1;
                    log.error("Last.fm rate limit exceeded; stopping this run early");
                    break;
                }
                Err(err) => {
                    failed += 1;
                    log.warn(&format!(
                        "Last.fm lookup failed for track {} \"{}\" / \"{}\": {err}",
                        track.id, artist, track.title
                    ));
                }
            }

            if (index + 1) % 50 == 0 {
                log.info(&format!(
                    "Last.fm progress: {}/{} tracks, {updated} updated, {skipped} skipped, {failed} failed",
                    index + 1,
                    tracks.len()
                ));
            }
            tokio::time::sleep(LASTFM_REQUEST_DELAY).await;
        }

        log.info(&format!(
            "Last.fm popularity update finished: {updated} updated, {skipped} skipped, {failed} failed, {} considered",
            tracks.len()
        ));
        Ok(())
    }
}

async fn fetch_track_info(
    client: &reqwest::Client,
    api_key: &str,
    artist: &str,
    track: &str,
) -> anyhow::Result<Option<(i64, i64)>> {
    let response = client
        .get("https://ws.audioscrobbler.com/2.0/")
        .query(&[
            ("method", "track.getInfo"),
            ("api_key", api_key),
            ("artist", artist),
            ("track", track),
            ("autocorrect", "1"),
            ("format", "json"),
        ])
        .send()
        .await?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response.error_for_status()?;
    let body: LastfmTrackInfoResponse = response.json().await?;
    if let Some(code) = body.error {
        if code == 29 {
            anyhow::bail!("Last.fm rate limit exceeded");
        }
        if code == 6 || code == 7 {
            return Ok(None);
        }
        anyhow::bail!(
            "Last.fm API error {code}: {}",
            body.message.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    let Some(info) = body.track else {
        return Ok(None);
    };
    let listeners = info
        .listeners
        .as_deref()
        .unwrap_or("0")
        .parse::<i64>()
        .unwrap_or(0);
    let playcount = info
        .playcount
        .as_deref()
        .unwrap_or("0")
        .parse::<i64>()
        .unwrap_or(0);
    Ok(Some((listeners.max(0), playcount.max(0))))
}

fn popularity_rating(listeners: i64, playcount: i64) -> f64 {
    let listeners = listeners.max(0) as f64;
    let playcount = playcount.max(0) as f64;
    playcount.ln_1p() * listeners.ln_1p()
}
