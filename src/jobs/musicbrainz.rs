use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;

const MUSICBRAINZ_BASE_URL: &str = "https://musicbrainz.org/ws/2";
const COVER_ART_ARCHIVE_BASE_URL: &str = "https://coverartarchive.org";
const MUSICBRAINZ_REQUEST_DELAY: Duration = Duration::from_millis(1100);
const MUSICBRAINZ_TAG_LIMIT: usize = 12;

#[derive(Debug, Clone)]
pub struct MusicBrainzTag {
    pub name: String,
    pub weight: f64,
}

#[derive(Debug, Clone)]
pub struct MusicBrainzArtistMatch {
    pub mbid: String,
    pub score: i32,
}

#[derive(Debug, Clone)]
pub struct MusicBrainzReleaseMatch {
    pub mbid: String,
    pub release_group_mbid: Option<String>,
    pub score: i32,
}

#[derive(Debug, Clone)]
pub struct MusicBrainzReleaseTags {
    pub release_group_mbid: Option<String>,
    pub tags: Vec<MusicBrainzTag>,
}

pub struct MusicBrainzClient {
    client: Client,
    last_musicbrainz_request: Mutex<Option<Instant>>,
}

pub async fn load_external_id(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
    id_kind: &str,
) -> anyhow::Result<Option<String>> {
    let value = sqlx::query_scalar::<_, String>(
        r#"SELECT external_id::text
           FROM furumusic__external_metadata_id
           WHERE entity_kind = $1
             AND entity_id = $2
             AND source = 'musicbrainz'
             AND id_kind = $3
           LIMIT 1"#,
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(id_kind)
    .fetch_optional(pool)
    .await?;
    Ok(value)
}

pub async fn save_external_id(
    pool: &sqlx::PgPool,
    entity_kind: &str,
    entity_id: i64,
    id_kind: &str,
    external_id: &str,
    confidence: f64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO furumusic__external_metadata_id
              (entity_kind, entity_id, source, id_kind, external_id, confidence, updated_at)
           VALUES ($1, $2, 'musicbrainz', $3, $4, $5, $6)
           ON CONFLICT (entity_kind, entity_id, source, id_kind) DO UPDATE SET
              external_id = EXCLUDED.external_id,
              confidence = EXCLUDED.confidence,
              updated_at = EXCLUDED.updated_at"#,
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(id_kind)
    .bind(external_id)
    .bind(confidence)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn load_or_search_release_mbid(
    pool: &sqlx::PgPool,
    client: &MusicBrainzClient,
    release_id: i64,
    artist_name: &str,
    release_title: &str,
    representative_track_title: Option<&str>,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let release_mbid = load_external_id(pool, "release", release_id, "release").await?;
    let release_group_mbid = load_external_id(pool, "release", release_id, "release_group").await?;
    if release_mbid.is_some() || release_group_mbid.is_some() {
        return Ok((release_mbid, release_group_mbid));
    }

    let found = match client.search_release(artist_name, release_title).await? {
        Some(found) => Some(found),
        None => {
            if let Some(track_title) =
                representative_track_title.filter(|value| !value.trim().is_empty())
            {
                client
                    .search_release_by_recording(artist_name, track_title)
                    .await?
            } else {
                None
            }
        }
    };

    let Some(found) = found else {
        return Ok((None, None));
    };
    save_external_id(
        pool,
        "release",
        release_id,
        "release",
        &found.mbid,
        found.score as f64 / 100.0,
    )
    .await?;
    if let Some(group_mbid) = found.release_group_mbid.as_deref() {
        save_external_id(
            pool,
            "release",
            release_id,
            "release_group",
            group_mbid,
            found.score as f64 / 100.0,
        )
        .await?;
    }
    Ok((Some(found.mbid), found.release_group_mbid))
}

impl MusicBrainzClient {
    pub fn new(user_agent_prefix: &str) -> anyhow::Result<Self> {
        let client = Client::builder()
            .user_agent(format!(
                "{}/{} (musicbrainz.org/doc/MusicBrainz_API)",
                user_agent_prefix,
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            client,
            last_musicbrainz_request: Mutex::new(None),
        })
    }

    pub fn http_client(&self) -> &Client {
        &self.client
    }

    pub async fn search_artist(
        &self,
        name: &str,
    ) -> anyhow::Result<Option<MusicBrainzArtistMatch>> {
        let query = format!("artist:\"{}\"", escape_search_value(name));
        let response: Option<ArtistSearchResponse> = self
            .get_musicbrainz_json(
                "artist",
                &[("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
            )
            .await?;
        let Some(response) = response else {
            return Ok(None);
        };
        Ok(response
            .artists
            .into_iter()
            .filter(|artist| artist.id.trim().len() == 36)
            .max_by_key(|artist| artist.score.unwrap_or(0))
            .and_then(|artist| {
                let score = artist.score.unwrap_or(0);
                (score >= 70).then_some(MusicBrainzArtistMatch {
                    mbid: artist.id,
                    score,
                })
            }))
    }

    pub async fn search_release(
        &self,
        artist: &str,
        title: &str,
    ) -> anyhow::Result<Option<MusicBrainzReleaseMatch>> {
        let query = format!(
            "release:\"{}\" AND artist:\"{}\"",
            escape_search_value(title),
            escape_search_value(artist)
        );
        let response: Option<ReleaseSearchResponse> = self
            .get_musicbrainz_json(
                "release",
                &[("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
            )
            .await?;
        let Some(response) = response else {
            return Ok(None);
        };
        Ok(response
            .releases
            .into_iter()
            .filter(|release| release.id.trim().len() == 36)
            .max_by_key(|release| release.score.unwrap_or(0))
            .and_then(|release| {
                let score = release.score.unwrap_or(0);
                (score >= 70).then_some(MusicBrainzReleaseMatch {
                    mbid: release.id,
                    release_group_mbid: release.release_group.map(|group| group.id),
                    score,
                })
            }))
    }

    pub async fn search_release_by_recording(
        &self,
        artist: &str,
        track_title: &str,
    ) -> anyhow::Result<Option<MusicBrainzReleaseMatch>> {
        let query = format!(
            "recording:\"{}\" AND artist:\"{}\"",
            escape_search_value(track_title),
            escape_search_value(artist)
        );
        let response: Option<RecordingSearchResponse> = self
            .get_musicbrainz_json(
                "recording",
                &[("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
            )
            .await?;
        let Some(response) = response else {
            return Ok(None);
        };

        Ok(response
            .recordings
            .into_iter()
            .flat_map(|recording| {
                let recording_score = recording.score.unwrap_or(0);
                recording
                    .releases
                    .into_iter()
                    .filter(move |release| release.id.trim().len() == 36)
                    .map(move |release| (recording_score, release))
            })
            .max_by_key(|(score, _)| *score)
            .and_then(|(score, release)| {
                (score >= 70).then_some(MusicBrainzReleaseMatch {
                    mbid: release.id,
                    release_group_mbid: release.release_group.map(|group| group.id),
                    score,
                })
            }))
    }

    pub async fn lookup_artist_tags(&self, mbid: &str) -> anyhow::Result<Vec<MusicBrainzTag>> {
        let response: Option<TaggedEntityResponse> = self
            .get_musicbrainz_json(
                &format!("artist/{mbid}"),
                &[("inc", "tags+genres"), ("fmt", "json")],
            )
            .await?;
        Ok(response.map(tags_from_entity).unwrap_or_default())
    }

    pub async fn lookup_release_tags(&self, mbid: &str) -> anyhow::Result<MusicBrainzReleaseTags> {
        let response: Option<ReleaseLookupResponse> = self
            .get_musicbrainz_json(
                &format!("release/{mbid}"),
                &[("inc", "tags+genres+release-groups"), ("fmt", "json")],
            )
            .await?;
        let Some(response) = response else {
            return Ok(MusicBrainzReleaseTags {
                release_group_mbid: None,
                tags: Vec::new(),
            });
        };

        let mut tags = tags_from_parts(response.tags, response.genres);
        let release_group_mbid = response
            .release_group
            .as_ref()
            .map(|group| group.id.clone());
        if let Some(group_mbid) = release_group_mbid.as_deref() {
            let group_response: Option<TaggedEntityResponse> = self
                .get_musicbrainz_json(
                    &format!("release-group/{group_mbid}"),
                    &[("inc", "tags+genres"), ("fmt", "json")],
                )
                .await?;
            merge_tags(
                &mut tags,
                group_response.map(tags_from_entity).unwrap_or_default(),
            );
        }
        tags.sort_by(|a, b| {
            b.weight
                .total_cmp(&a.weight)
                .then_with(|| a.name.cmp(&b.name))
        });
        tags.truncate(MUSICBRAINZ_TAG_LIMIT);

        Ok(MusicBrainzReleaseTags {
            release_group_mbid,
            tags,
        })
    }

    pub async fn fetch_cover_art_front_url(
        &self,
        release_mbid: Option<&str>,
        release_group_mbid: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        if let Some(mbid) = release_mbid {
            if let Some(url) = self.cover_art_front_url("release", mbid).await? {
                return Ok(Some(url));
            }
        }
        if let Some(mbid) = release_group_mbid {
            if let Some(url) = self.cover_art_front_url("release-group", mbid).await? {
                return Ok(Some(url));
            }
        }
        Ok(None)
    }

    async fn get_musicbrainz_json<T>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> anyhow::Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.wait_for_musicbrainz_slot().await;
        let url = format!("{MUSICBRAINZ_BASE_URL}/{}", path.trim_start_matches('/'));
        let response = self.client.get(url).query(query).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() == StatusCode::TOO_MANY_REQUESTS
            || response.status() == StatusCode::SERVICE_UNAVAILABLE
        {
            anyhow::bail!(
                "MusicBrainz rate limit or service unavailable: {}",
                response.status()
            );
        }
        let response = response.error_for_status()?;
        Ok(Some(response.json::<T>().await?))
    }

    async fn cover_art_front_url(&self, kind: &str, mbid: &str) -> anyhow::Result<Option<String>> {
        let url = format!("{COVER_ART_ARCHIVE_BASE_URL}/{kind}/{mbid}");
        let response = self.client.get(url).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() == StatusCode::TOO_MANY_REQUESTS
            || response.status() == StatusCode::SERVICE_UNAVAILABLE
        {
            anyhow::bail!("Cover Art Archive unavailable: {}", response.status());
        }
        let response = response.error_for_status()?;
        let body = response.json::<CoverArtArchiveResponse>().await?;
        Ok(best_cover_art_url(body.images))
    }

    async fn wait_for_musicbrainz_slot(&self) {
        let mut last = self.last_musicbrainz_request.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < MUSICBRAINZ_REQUEST_DELAY {
                tokio::time::sleep(MUSICBRAINZ_REQUEST_DELAY - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[derive(Debug, Deserialize)]
struct ArtistSearchResponse {
    #[serde(default)]
    artists: Vec<ArtistSearchItem>,
}

#[derive(Debug, Deserialize)]
struct ArtistSearchItem {
    id: String,
    score: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct ReleaseSearchResponse {
    #[serde(default)]
    releases: Vec<ReleaseSearchItem>,
}

#[derive(Debug, Deserialize)]
struct ReleaseSearchItem {
    id: String,
    score: Option<i32>,
    #[serde(rename = "release-group")]
    release_group: Option<MusicBrainzIdRef>,
}

#[derive(Debug, Deserialize)]
struct RecordingSearchResponse {
    #[serde(default)]
    recordings: Vec<RecordingSearchItem>,
}

#[derive(Debug, Deserialize)]
struct RecordingSearchItem {
    score: Option<i32>,
    #[serde(default)]
    releases: Vec<RecordingReleaseItem>,
}

#[derive(Debug, Deserialize)]
struct RecordingReleaseItem {
    id: String,
    #[serde(rename = "release-group")]
    release_group: Option<MusicBrainzIdRef>,
}

#[derive(Debug, Deserialize)]
struct ReleaseLookupResponse {
    #[serde(default)]
    tags: Vec<MusicBrainzTagItem>,
    #[serde(default)]
    genres: Vec<MusicBrainzTagItem>,
    #[serde(rename = "release-group")]
    release_group: Option<MusicBrainzIdRef>,
}

#[derive(Debug, Deserialize)]
struct TaggedEntityResponse {
    #[serde(default)]
    tags: Vec<MusicBrainzTagItem>,
    #[serde(default)]
    genres: Vec<MusicBrainzTagItem>,
}

#[derive(Debug, Deserialize)]
struct MusicBrainzIdRef {
    id: String,
}

#[derive(Debug, Deserialize)]
struct MusicBrainzTagItem {
    name: String,
    count: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CoverArtArchiveResponse {
    #[serde(default)]
    images: Vec<CoverArtArchiveImage>,
}

#[derive(Debug, Deserialize)]
struct CoverArtArchiveImage {
    image: Option<String>,
    front: Option<bool>,
    approved: Option<bool>,
    #[serde(default)]
    types: Vec<String>,
    thumbnails: Option<CoverArtArchiveThumbnails>,
}

#[derive(Debug, Deserialize)]
struct CoverArtArchiveThumbnails {
    #[serde(rename = "1200")]
    size_1200: Option<String>,
    #[serde(rename = "500")]
    size_500: Option<String>,
    large: Option<String>,
}

fn escape_search_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn tags_from_entity(entity: TaggedEntityResponse) -> Vec<MusicBrainzTag> {
    tags_from_parts(entity.tags, entity.genres)
}

fn tags_from_parts(
    tags: Vec<MusicBrainzTagItem>,
    genres: Vec<MusicBrainzTagItem>,
) -> Vec<MusicBrainzTag> {
    let mut result = Vec::new();
    merge_items(&mut result, genres, 2.0);
    merge_items(&mut result, tags, 1.0);
    result.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.name.cmp(&b.name))
    });
    result.truncate(MUSICBRAINZ_TAG_LIMIT);
    result
}

fn merge_items(result: &mut Vec<MusicBrainzTag>, items: Vec<MusicBrainzTagItem>, multiplier: f64) {
    for item in items {
        let name = item.name.trim();
        if name.is_empty() {
            continue;
        }
        let weight = item.count.unwrap_or(1).max(1) as f64 * multiplier;
        if let Some(existing) = result
            .iter_mut()
            .find(|tag| tag.name.eq_ignore_ascii_case(name))
        {
            existing.weight = existing.weight.max(weight);
        } else {
            result.push(MusicBrainzTag {
                name: name.to_string(),
                weight,
            });
        }
    }
}

fn merge_tags(result: &mut Vec<MusicBrainzTag>, extra: Vec<MusicBrainzTag>) {
    for tag in extra {
        if let Some(existing) = result
            .iter_mut()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(&tag.name))
        {
            existing.weight = existing.weight.max(tag.weight);
        } else {
            result.push(tag);
        }
    }
}

fn best_cover_art_url(mut images: Vec<CoverArtArchiveImage>) -> Option<String> {
    images.sort_by_key(|image| {
        let front = image.front.unwrap_or(false)
            || image
                .types
                .iter()
                .any(|value| value.eq_ignore_ascii_case("front"));
        let approved = image.approved.unwrap_or(false);
        (u8::from(front), u8::from(approved))
    });
    images
        .into_iter()
        .rev()
        .find_map(|image| {
            image
                .thumbnails
                .as_ref()
                .and_then(|thumbs| {
                    thumbs
                        .size_1200
                        .as_deref()
                        .or(thumbs.size_500.as_deref())
                        .or(thumbs.large.as_deref())
                })
                .map(str::to_string)
                .or(image.image)
        })
        .filter(|url| !url.trim().is_empty())
}
