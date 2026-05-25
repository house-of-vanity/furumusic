use std::sync::Arc;

use cot::db::Database;
use cot::http::StatusCode;
use cot::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use cot::json::Json;
use cot::request::extractors::Path;
use cot::response::IntoResponse;
use cot::router::method::{get, post};
use cot::router::{Route, Router};
use cot::session::Session;
use cot::{App, Body, Template};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::config::AppConfig;
use crate::i18n::Translations;
use crate::scheduler::SchedulerHandle;
use crate::torrents::{TorrentPreviewRequest, TorrentService, TorrentStartRequest};

// ---------------------------------------------------------------------------
// JSON error helper
// ---------------------------------------------------------------------------

fn json_error(status: StatusCode, message: &str) -> cot::response::Response {
    let body = serde_json::json!({ "error": message });
    cot::http::Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::fixed(body.to_string()))
        .expect("valid response")
}

// ---------------------------------------------------------------------------
// DTO structs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
struct ArtistCard {
    id: i64,
    name: String,
    image_url: Option<String>,
    release_count: i64,
    track_count: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Paginated<T: Serialize> {
    items: Vec<T>,
    total: i64,
    page: i32,
    per_page: i32,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ReleaseCard {
    id: i64,
    title: String,
    release_type: String,
    year: Option<i32>,
    cover_url: Option<String>,
    track_count: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ArtistDetail {
    id: i64,
    name: String,
    image_url: Option<String>,
    total_track_count: i64,
    total_play_count: i64,
    releases: Vec<ReleaseCard>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ArtistRef {
    id: i64,
    name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct TrackItem {
    id: i64,
    title: String,
    track_number: Option<i32>,
    disc_number: Option<i32>,
    duration_seconds: f64,
    artists: Vec<ArtistRef>,
    featured_artists: Vec<ArtistRef>,
    cover_url: Option<String>,
    stream_url: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ReleaseDetail {
    id: i64,
    title: String,
    release_type: String,
    year: Option<i32>,
    cover_url: Option<String>,
    artists: Vec<ArtistRef>,
    tracks: Vec<TrackItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PlaylistCard {
    id: i64,
    title: String,
    track_count: i64,
    is_own: bool,
    kind: String, // "user" or "likes"
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct PlaybackStateDto {
    current_track_id: Option<i64>,
    position_ms: i32,
    queue: Vec<i64>,
    queue_position: i32,
    shuffle: bool,
    repeat_mode: String,
    volume: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PlaylistDetail {
    id: i64,
    title: String,
    description: Option<String>,
    is_own: bool,
    kind: String,
    tracks: Vec<TrackItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchResults {
    artists: Vec<ArtistCard>,
    releases: Vec<ReleaseCard>,
    tracks: Vec<TrackItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct UserStats {
    liked_tracks: i64,
    playlists: i64,
    plays: i64,
    listened_minutes: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct UserProfile {
    name: String,
    role: String,
    stats: UserStats,
}

#[derive(Debug, Deserialize)]
struct HistoryEntry {
    track_id: i64,
    duration_listened: Option<i32>,
    completed: bool,
}

#[derive(Debug, Deserialize)]
struct TracksByIdsRequest {
    ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct CreatePlaylistRequest {
    title: String,
}

#[derive(Debug, Deserialize)]
struct UpdatePlaylistRequest {
    title: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddTracksRequest {
    track_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct RemoveTrackRequest {
    track_id: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LikeStatus {
    liked: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LikedIds {
    track_ids: Vec<i64>,
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PaginationQuery {
    page: Option<i32>,
    limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct PathId {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct PathStringId {
    id: String,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct PathTrackId {
    track_id: i64,
}

#[derive(Debug, Deserialize)]
struct PathMediaFileId {
    media_file_id: i64,
}

// ---------------------------------------------------------------------------
// sqlx row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ArtistRow {
    id: i64,
    name: String,
    image_file_id: Option<i64>,
    release_count: i64,
    track_count: i64,
}

#[derive(sqlx::FromRow)]
struct CountRow {
    count: i64,
}

#[derive(sqlx::FromRow)]
struct ReleaseRow {
    id: i64,
    title: String,
    release_type: String,
    year: Option<i32>,
    cover_file_id: Option<i64>,
    track_count: i64,
}

#[derive(sqlx::FromRow)]
struct ArtistBriefRow {
    id: i64,
    name: String,
}

#[derive(sqlx::FromRow)]
struct TrackRow {
    id: i64,
    title: String,
    track_number: Option<i32>,
    disc_number: Option<i32>,
    duration_seconds: f64,
    cover_file_id: Option<i64>,
    release_cover_file_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct TrackArtistRow {
    track_id: i64,
    artist_id: i64,
    artist_name: String,
    role: String,
}

#[derive(sqlx::FromRow)]
struct MediaFileRow {
    file_path: String,
    mime_type: String,
    file_size_bytes: i64,
}

#[derive(sqlx::FromRow)]
struct PlaybackStateRow {
    current_track_id: Option<i64>,
    position_ms: i32,
    queue_json: String,
    queue_position: i32,
    shuffle: bool,
    repeat_mode: String,
    volume: f64,
}

#[derive(sqlx::FromRow)]
struct PlaylistRow {
    id: i64,
    title: String,
    track_count: i64,
    is_own: bool,
}

#[derive(sqlx::FromRow)]
struct PlaylistInfoRow {
    id: i64,
    title: String,
    description: Option<String>,
    owner_id: i64,
}

#[derive(sqlx::FromRow)]
struct PlaylistTrackRow {
    id: i64,
    title: String,
    track_number: Option<i32>,
    disc_number: Option<i32>,
    duration_seconds: f64,
    cover_file_id: Option<i64>,
    release_cover_file_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct SearchArtistRow {
    id: i64,
    name: String,
    image_file_id: Option<i64>,
    release_count: i64,
    track_count: i64,
}

#[derive(sqlx::FromRow)]
struct SearchReleaseRow {
    id: i64,
    title: String,
    release_type: String,
    year: Option<i32>,
    cover_file_id: Option<i64>,
    track_count: i64,
}

#[derive(sqlx::FromRow)]
struct SearchTrackRow {
    id: i64,
    title: String,
    track_number: Option<i32>,
    disc_number: Option<i32>,
    duration_seconds: f64,
    cover_file_id: Option<i64>,
    release_cover_file_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ReleaseInfoRow {
    id: i64,
    title: String,
    release_type: String,
    year: Option<i32>,
    cover_file_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cover_url(file_id: Option<i64>) -> Option<String> {
    file_id.map(|id| format!("/api/player/cover/{id}"))
}

fn track_cover_url(track_cover: Option<i64>, release_cover: Option<i64>) -> Option<String> {
    cover_url(track_cover.or(release_cover))
}

// ---------------------------------------------------------------------------
// SPA shell
// ---------------------------------------------------------------------------

#[derive(Debug, Template)]
#[template(path = "player.html")]
pub struct PlayerPageTemplate {
    pub t: &'static Translations,
}

// ---------------------------------------------------------------------------
// GET /api/player/me
// ---------------------------------------------------------------------------

async fn me_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let liked_tracks: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM furumusic__user_liked_track WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    let playlists: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM furumusic__playlist WHERE owner_id = $1")
            .bind(user.id)
            .fetch_one(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    let plays: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM furumusic__play_history WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    let listened_seconds: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(duration_listened), 0) FROM furumusic__play_history WHERE user_id = $1",
    )
    .bind(user.id)
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(UserProfile {
        name: user.name,
        role: user.role.code().to_string(),
        stats: UserStats {
            liked_tracks: liked_tracks.0,
            playlists: playlists.0,
            plays: plays.0,
            listened_minutes: listened_seconds.unwrap_or(0) / 60,
        },
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/artists?page=N&limit=N
// ---------------------------------------------------------------------------

async fn artists_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    query: cot::request::extractors::UrlQuery<PaginationQuery>,
) -> cot::Result<cot::response::Response> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let page = query.0.page.unwrap_or(1).max(1);
    let per_page = query.0.limit.unwrap_or(60).clamp(1, 200);
    let offset = (page - 1) as i64 * per_page as i64;

    let total_row = sqlx::query_as::<_, CountRow>(
        r#"SELECT COUNT(DISTINCT a.id) AS count
           FROM furumusic__artist a
           JOIN furumusic__release_artist ra ON ra.artist_id = a.id
           JOIN furumusic__release r ON r.id = ra.release_id
           WHERE a.is_hidden = false AND r.is_hidden = false"#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let rows = sqlx::query_as::<_, ArtistRow>(
        r#"SELECT a.id, a.name::text as name, a.image_file_id,
                  s.release_count,
                  s.track_count
           FROM furumusic__artist a
           JOIN (
               SELECT ra.artist_id,
                      COUNT(DISTINCT r.id) AS release_count,
                      COUNT(t.id) AS track_count
               FROM furumusic__release_artist ra
               JOIN furumusic__release r ON r.id = ra.release_id AND r.is_hidden = false
               LEFT JOIN furumusic__track t ON t.release_id = r.id AND t.is_hidden = false
               GROUP BY ra.artist_id
           ) s ON s.artist_id = a.id
           WHERE a.is_hidden = false
           ORDER BY s.release_count DESC, s.track_count DESC, a.name_sort
           LIMIT $1 OFFSET $2"#,
    )
    .bind(per_page as i64)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let items: Vec<ArtistCard> = rows
        .into_iter()
        .map(|r| ArtistCard {
            id: r.id,
            name: r.name,
            image_url: cover_url(r.image_file_id),
            release_count: r.release_count,
            track_count: r.track_count,
        })
        .collect();

    Json(Paginated {
        items,
        total: total_row.count,
        page,
        per_page,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/artists/{id}
// ---------------------------------------------------------------------------

async fn artist_detail_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
) -> cot::Result<cot::response::Response> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let artist_id = path.0.id;

    let artist = sqlx::query_as::<_, ArtistBriefRow>(
        "SELECT id, name::text as name FROM furumusic__artist WHERE id = $1 AND is_hidden = false",
    )
    .bind(artist_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(artist) = artist else {
        return Ok(json_error(StatusCode::NOT_FOUND, "artist not found"));
    };

    let image_file_id: Option<i64> =
        sqlx::query_scalar("SELECT image_file_id FROM furumusic__artist WHERE id = $1")
            .bind(artist_id)
            .fetch_one(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    let releases = sqlx::query_as::<_, ReleaseRow>(
        r#"SELECT r.id, r.title::text as title, r.release_type::text as release_type,
                  r.year, r.cover_file_id,
                  COALESCE((SELECT COUNT(*) FROM furumusic__track t WHERE t.release_id = r.id AND t.is_hidden = false), 0) as track_count
           FROM furumusic__release r
           JOIN furumusic__release_artist ra ON ra.release_id = r.id
           WHERE ra.artist_id = $1 AND r.is_hidden = false
           ORDER BY r.year DESC NULLS LAST, r.title_sort"#,
    )
    .bind(artist_id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let release_cards: Vec<ReleaseCard> = releases
        .into_iter()
        .map(|r| ReleaseCard {
            id: r.id,
            title: r.title,
            release_type: r.release_type,
            year: r.year,
            cover_url: cover_url(r.cover_file_id),
            track_count: r.track_count,
        })
        .collect();

    let total_track_count = release_cards.iter().map(|r| r.track_count).sum();
    let total_play_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)
           FROM furumusic__play_history ph
           JOIN furumusic__track t ON t.id = ph.track_id
           JOIN furumusic__release_artist ra ON ra.release_id = t.release_id
           JOIN furumusic__release r ON r.id = t.release_id
           WHERE ra.artist_id = $1 AND t.is_hidden = false AND r.is_hidden = false"#,
    )
    .bind(artist_id)
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(ArtistDetail {
        id: artist.id,
        name: artist.name,
        image_url: cover_url(image_file_id),
        total_track_count,
        total_play_count,
        releases: release_cards,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/releases/{id}
// ---------------------------------------------------------------------------

async fn release_detail_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
) -> cot::Result<cot::response::Response> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let release_id = path.0.id;

    let release = sqlx::query_as::<_, ReleaseInfoRow>(
        r#"SELECT id, title::text as title, release_type::text as release_type, year, cover_file_id
           FROM furumusic__release WHERE id = $1 AND is_hidden = false"#,
    )
    .bind(release_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(release) = release else {
        return Ok(json_error(StatusCode::NOT_FOUND, "release not found"));
    };

    // Release artists
    let release_artists = sqlx::query_as::<_, ArtistBriefRow>(
        r#"SELECT a.id, a.name::text as name
           FROM furumusic__artist a
           JOIN furumusic__release_artist ra ON ra.artist_id = a.id
           WHERE ra.release_id = $1
           ORDER BY ra.position"#,
    )
    .bind(release_id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    // Tracks
    let tracks = sqlx::query_as::<_, TrackRow>(
        r#"SELECT t.id, t.title::text as title, t.track_number, t.disc_number,
                  t.duration_seconds, t.cover_file_id,
                  r.cover_file_id as release_cover_file_id
           FROM furumusic__track t
           JOIN furumusic__release r ON r.id = t.release_id
           WHERE t.release_id = $1 AND t.is_hidden = false
           ORDER BY t.disc_number NULLS FIRST, t.track_number NULLS LAST"#,
    )
    .bind(release_id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let track_ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    // Track artists (batch)
    let track_artists = if track_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TrackArtistRow>(
            r#"SELECT ta.track_id, ta.artist_id, a.name::text as artist_name, ta.role::text as role
               FROM furumusic__track_artist ta
               JOIN furumusic__artist a ON a.id = ta.artist_id
               WHERE ta.track_id = ANY($1)
               ORDER BY ta.track_id, ta.position"#,
        )
        .bind(&track_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?
    };

    // Group track artists
    let mut track_main_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();
    let mut track_feat_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();

    for ta in &track_artists {
        let artist_ref = ArtistRef {
            id: ta.artist_id,
            name: ta.artist_name.clone(),
        };
        if ta.role == "featuring" {
            track_feat_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        } else {
            track_main_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        }
    }

    let track_items: Vec<TrackItem> = tracks
        .into_iter()
        .map(|t| {
            let tid = t.id;
            TrackItem {
                id: t.id,
                title: t.title,
                track_number: t.track_number,
                disc_number: t.disc_number,
                duration_seconds: t.duration_seconds,
                artists: track_main_artists.remove(&tid).unwrap_or_default(),
                featured_artists: track_feat_artists.remove(&tid).unwrap_or_default(),
                cover_url: track_cover_url(t.cover_file_id, t.release_cover_file_id),
                stream_url: format!("/api/player/stream/{tid}"),
            }
        })
        .collect();

    Json(ReleaseDetail {
        id: release.id,
        title: release.title,
        release_type: release.release_type,
        year: release.year,
        cover_url: cover_url(release.cover_file_id),
        artists: release_artists
            .into_iter()
            .map(|a| ArtistRef {
                id: a.id,
                name: a.name,
            })
            .collect(),
        tracks: track_items,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/playlists
// ---------------------------------------------------------------------------

async fn playlists_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    // Count liked tracks for the virtual Likes playlist
    let likes_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM furumusic__user_liked_track WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    let mut cards = vec![PlaylistCard {
        id: -1,
        title: "Likes".to_string(),
        track_count: likes_count.0,
        is_own: true,
        kind: "likes".to_string(),
    }];

    let rows = sqlx::query_as::<_, PlaylistRow>(
        r#"SELECT p.id, p.title::text as title,
                  COALESCE((SELECT COUNT(*) FROM furumusic__playlist_track pt WHERE pt.playlist_id = p.id), 0) as track_count,
                  (p.owner_id = $1) as is_own
           FROM furumusic__playlist p
           WHERE p.owner_id = $1
              OR p.id IN (SELECT sp.playlist_id FROM furumusic__saved_playlist sp WHERE sp.user_id = $1)
              OR p.is_public = true
           ORDER BY p.title"#,
    )
    .bind(user.id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    cards.extend(rows.into_iter().map(|r| PlaylistCard {
        id: r.id,
        title: r.title,
        track_count: r.track_count,
        is_own: r.is_own,
        kind: "user".to_string(),
    }));

    Json(cards).into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/playlists/{id}
// ---------------------------------------------------------------------------

async fn playlist_detail_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let playlist_id = path.0.id;

    // Virtual Likes playlist (id = -1)
    if playlist_id == -1 {
        return likes_playlist_handler(user.id, pool).await;
    }

    let info = sqlx::query_as::<_, PlaylistInfoRow>(
        "SELECT id, title::text as title, description, owner_id FROM furumusic__playlist WHERE id = $1",
    )
    .bind(playlist_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(info) = info else {
        return Ok(json_error(StatusCode::NOT_FOUND, "playlist not found"));
    };

    let tracks = sqlx::query_as::<_, PlaylistTrackRow>(
        r#"SELECT t.id, t.title::text as title, t.track_number, t.disc_number,
                  t.duration_seconds, t.cover_file_id,
                  r.cover_file_id as release_cover_file_id
           FROM furumusic__playlist_track pt
           JOIN furumusic__track t ON t.id = pt.track_id
           JOIN furumusic__release r ON r.id = t.release_id
           WHERE pt.playlist_id = $1 AND t.is_hidden = false
           ORDER BY pt.position"#,
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let track_items = build_track_items(tracks, pool).await?;

    Json(PlaylistDetail {
        id: info.id,
        title: info.title,
        description: info.description,
        is_own: info.owner_id == user.id,
        kind: "user".to_string(),
        tracks: track_items,
    })
    .into_response()
}

/// Shared helper: given PlaylistTrackRows, fetch artists and build TrackItems.
async fn build_track_items(
    tracks: Vec<PlaylistTrackRow>,
    pool: &sqlx::PgPool,
) -> cot::Result<Vec<TrackItem>> {
    let track_ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    let track_artists = if track_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TrackArtistRow>(
            r#"SELECT ta.track_id, ta.artist_id, a.name::text as artist_name, ta.role::text as role
               FROM furumusic__track_artist ta
               JOIN furumusic__artist a ON a.id = ta.artist_id
               WHERE ta.track_id = ANY($1)
               ORDER BY ta.track_id, ta.position"#,
        )
        .bind(&track_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?
    };

    let mut track_main_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();
    let mut track_feat_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();

    for ta in &track_artists {
        let artist_ref = ArtistRef {
            id: ta.artist_id,
            name: ta.artist_name.clone(),
        };
        if ta.role == "featuring" {
            track_feat_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        } else {
            track_main_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        }
    }

    Ok(tracks
        .into_iter()
        .map(|t| {
            let tid = t.id;
            TrackItem {
                id: t.id,
                title: t.title,
                track_number: t.track_number,
                disc_number: t.disc_number,
                duration_seconds: t.duration_seconds,
                artists: track_main_artists.remove(&tid).unwrap_or_default(),
                featured_artists: track_feat_artists.remove(&tid).unwrap_or_default(),
                cover_url: track_cover_url(t.cover_file_id, t.release_cover_file_id),
                stream_url: format!("/api/player/stream/{tid}"),
            }
        })
        .collect())
}

/// Return the virtual "Likes" playlist for a given user.
async fn likes_playlist_handler(
    user_id: i64,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let tracks = sqlx::query_as::<_, PlaylistTrackRow>(
        r#"SELECT t.id, t.title::text as title, t.track_number, t.disc_number,
                  t.duration_seconds, t.cover_file_id,
                  r.cover_file_id as release_cover_file_id
           FROM furumusic__user_liked_track ult
           JOIN furumusic__track t ON t.id = ult.track_id
           JOIN furumusic__release r ON r.id = t.release_id
           WHERE ult.user_id = $1 AND t.is_hidden = false
           ORDER BY ult.created_at DESC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let track_items = build_track_items(tracks, pool).await?;

    Json(PlaylistDetail {
        id: -1,
        title: "Likes".to_string(),
        description: None,
        is_own: true,
        kind: "likes".to_string(),
        tracks: track_items,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/stream/{track_id}  — Range-aware audio streaming
// ---------------------------------------------------------------------------

async fn stream_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    config: &AppConfig,
    request: &cot::http::Request<Body>,
    path: Path<PathTrackId>,
) -> cot::Result<cot::http::Response<Body>> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let track_id = path.0.track_id;

    // Look up track → audio_file_id → MediaFile
    let media = sqlx::query_as::<_, MediaFileRow>(
        r#"SELECT mf.file_path, mf.mime_type::text as mime_type, mf.file_size_bytes
           FROM furumusic__track t
           JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
           WHERE t.id = $1"#,
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(media) = media else {
        return Ok(json_error(StatusCode::NOT_FOUND, "track not found"));
    };

    let full_path = std::path::Path::new(&config.agent_storage_dir).join(&media.file_path);

    if !full_path.exists() {
        return Ok(json_error(
            StatusCode::NOT_FOUND,
            "audio file not found on disk",
        ));
    }

    let file_size = media.file_size_bytes as u64;

    // Parse Range header
    let range_header = request.headers().get(RANGE).and_then(|v| v.to_str().ok());

    if let Some(range_str) = range_header {
        // Parse "bytes=START-END" or "bytes=START-"
        if let Some(range) = parse_range(range_str, file_size) {
            let (start, end) = range;
            let chunk_size = end - start + 1;

            let data = read_file_range(&full_path, start, chunk_size).await?;

            let response = cot::http::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(CONTENT_TYPE, media.mime_type.as_str())
                .header(ACCEPT_RANGES, "bytes")
                .header(CONTENT_RANGE, format!("bytes {start}-{end}/{file_size}"))
                .header(CONTENT_LENGTH, chunk_size.to_string())
                .body(Body::fixed(data))
                .expect("valid response");

            return Ok(response);
        }
    }

    // No Range or invalid range: return full file
    let data = tokio::fs::read(&full_path)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let response = cot::http::Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, media.mime_type.as_str())
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_LENGTH, file_size.to_string())
        .body(Body::fixed(data))
        .expect("valid response");

    Ok(response)
}

fn parse_range(header: &str, file_size: u64) -> Option<(u64, u64)> {
    let bytes_prefix = "bytes=";
    if !header.starts_with(bytes_prefix) {
        return None;
    }
    let range_spec = &header[bytes_prefix.len()..];
    let parts: Vec<&str> = range_spec.splitn(2, '-').collect();
    if parts.len() != 2 {
        return None;
    }

    let start: u64 = if parts[0].is_empty() {
        // Suffix range: bytes=-N means last N bytes
        let suffix: u64 = parts[1].parse().ok()?;
        file_size.saturating_sub(suffix)
    } else {
        parts[0].parse().ok()?
    };

    let end: u64 = if parts[1].is_empty() || parts[0].is_empty() {
        file_size - 1
    } else {
        parts[1].parse::<u64>().ok()?.min(file_size - 1)
    };

    if start > end || start >= file_size {
        return None;
    }

    Some((start, end))
}

async fn read_file_range(path: &std::path::Path, start: u64, length: u64) -> cot::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    Ok(buf)
}

// ---------------------------------------------------------------------------
// GET /api/player/cover/{media_file_id}
// ---------------------------------------------------------------------------

async fn cover_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    config: &AppConfig,
    path: Path<PathMediaFileId>,
) -> cot::Result<cot::http::Response<Body>> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let media_file_id = path.0.media_file_id;

    let media = sqlx::query_as::<_, MediaFileRow>(
        "SELECT file_path, mime_type::text as mime_type, file_size_bytes FROM furumusic__media_file WHERE id = $1",
    )
    .bind(media_file_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(media) = media else {
        return Ok(json_error(StatusCode::NOT_FOUND, "media file not found"));
    };

    let full_path = std::path::Path::new(&config.agent_storage_dir).join(&media.file_path);

    if !full_path.exists() {
        return Ok(json_error(StatusCode::NOT_FOUND, "file not found on disk"));
    }

    let data = tokio::fs::read(&full_path)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let response = cot::http::Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, media.mime_type.as_str())
        .header(CONTENT_LENGTH, data.len().to_string())
        .header("Cache-Control", "public, max-age=86400")
        .body(Body::fixed(data))
        .expect("valid response");

    Ok(response)
}

// ---------------------------------------------------------------------------
// GET /api/player/state
// ---------------------------------------------------------------------------

async fn get_state_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let state = sqlx::query_as::<_, PlaybackStateRow>(
        r#"SELECT current_track_id, position_ms, queue_json, queue_position, shuffle, repeat_mode::text as repeat_mode, volume
           FROM furumusic__playback_state WHERE user_id = $1"#,
    )
    .bind(user.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let dto = match state {
        Some(s) => {
            let queue: Vec<i64> = serde_json::from_str(&s.queue_json).unwrap_or_default();
            PlaybackStateDto {
                current_track_id: s.current_track_id,
                position_ms: s.position_ms,
                queue,
                queue_position: s.queue_position,
                shuffle: s.shuffle,
                repeat_mode: s.repeat_mode,
                volume: s.volume,
            }
        }
        None => PlaybackStateDto {
            current_track_id: None,
            position_ms: 0,
            queue: Vec::new(),
            queue_position: 0,
            shuffle: false,
            repeat_mode: "off".to_string(),
            volume: 0.7,
        },
    };

    Json(dto).into_response()
}

// ---------------------------------------------------------------------------
// PUT /api/player/state
// ---------------------------------------------------------------------------

async fn put_state_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    Json(dto): Json<PlaybackStateDto>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let queue_json =
        serde_json::to_string(&dto.queue).map_err(|e| cot::Error::internal(e.to_string()))?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    sqlx::query(
        r#"INSERT INTO furumusic__playback_state (user_id, current_track_id, position_ms, queue_json, queue_position, shuffle, repeat_mode, volume, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           ON CONFLICT (user_id) DO UPDATE SET
             current_track_id = $2, position_ms = $3, queue_json = $4,
             queue_position = $5, shuffle = $6, repeat_mode = $7, volume = $8, updated_at = $9"#,
    )
    .bind(user.id)
    .bind(dto.current_track_id)
    .bind(dto.position_ms)
    .bind(&queue_json)
    .bind(dto.queue_position)
    .bind(dto.shuffle)
    .bind(&dto.repeat_mode)
    .bind(dto.volume)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// POST /api/player/history
// ---------------------------------------------------------------------------

async fn history_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    Json(entry): Json<HistoryEntry>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    sqlx::query(
        r#"INSERT INTO furumusic__play_history (user_id, track_id, played_at, duration_listened, completed)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(user.id)
    .bind(entry.track_id)
    .bind(&now)
    .bind(entry.duration_listened)
    .bind(entry.completed)
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/search?q=...&limit=N
// ---------------------------------------------------------------------------

async fn search_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    query: cot::request::extractors::UrlQuery<SearchQuery>,
) -> cot::Result<cot::response::Response> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let q = query.0.q.trim().to_lowercase();
    if q.is_empty() {
        return Json(SearchResults {
            artists: Vec::new(),
            releases: Vec::new(),
            tracks: Vec::new(),
        })
        .into_response();
    }

    let limit = query.0.limit.unwrap_or(10).clamp(1, 50) as i64;
    let short = q.chars().count() < 3;

    let (artist_rows, release_rows, track_rows) = if short {
        let a = sqlx::query_as::<_, SearchArtistRow>(
            r#"SELECT a.id, a.name::text AS name, a.image_file_id,
                      COALESCE((SELECT COUNT(*) FROM furumusic__release_artist ra
                                JOIN furumusic__release r ON r.id = ra.release_id AND r.is_hidden = false
                                WHERE ra.artist_id = a.id), 0) AS release_count,
                      COALESCE((SELECT COUNT(*) FROM furumusic__release_artist ra
                                JOIN furumusic__release r ON r.id = ra.release_id AND r.is_hidden = false
                                JOIN furumusic__track t ON t.release_id = r.id AND t.is_hidden = false
                                WHERE ra.artist_id = a.id), 0) AS track_count
               FROM furumusic__artist a
               WHERE a.is_hidden = false AND a.name_sort ILIKE '%' || $1 || '%'
               ORDER BY a.name_sort LIMIT $2"#,
        )
        .bind(&q)
        .bind(limit)
        .fetch_all(pool);

        let r = sqlx::query_as::<_, SearchReleaseRow>(
            r#"SELECT r.id, r.title::text AS title, r.release_type::text AS release_type,
                      r.year, r.cover_file_id,
                      COALESCE((SELECT COUNT(*) FROM furumusic__track t WHERE t.release_id = r.id AND t.is_hidden = false), 0) AS track_count
               FROM furumusic__release r
               WHERE r.is_hidden = false AND r.title_sort ILIKE '%' || $1 || '%'
               ORDER BY r.title_sort LIMIT $2"#,
        )
        .bind(&q)
        .bind(limit)
        .fetch_all(pool);

        let t = sqlx::query_as::<_, SearchTrackRow>(
            r#"SELECT t.id, t.title::text AS title, t.track_number, t.disc_number,
                      t.duration_seconds, t.cover_file_id,
                      rel.cover_file_id AS release_cover_file_id
               FROM furumusic__track t
               JOIN furumusic__release rel ON rel.id = t.release_id
               WHERE t.is_hidden = false AND t.title_sort ILIKE '%' || $1 || '%'
               ORDER BY t.title_sort LIMIT $2"#,
        )
        .bind(&q)
        .bind(limit)
        .fetch_all(pool);

        tokio::try_join!(a, r, t).map_err(|e| cot::Error::internal(e.to_string()))?
    } else {
        let a = sqlx::query_as::<_, SearchArtistRow>(
            r#"SELECT id, name, image_file_id, release_count, track_count FROM (
                SELECT a.id, a.name::text AS name, a.image_file_id,
                       COALESCE((SELECT COUNT(*) FROM furumusic__release_artist ra
                                 JOIN furumusic__release r ON r.id = ra.release_id AND r.is_hidden = false
                                 WHERE ra.artist_id = a.id), 0) AS release_count,
                       COALESCE((SELECT COUNT(*) FROM furumusic__release_artist ra
                                 JOIN furumusic__release r ON r.id = ra.release_id AND r.is_hidden = false
                                 JOIN furumusic__track t ON t.release_id = r.id AND t.is_hidden = false
                                 WHERE ra.artist_id = a.id), 0) AS track_count,
                       MAX(sim) AS similarity
                FROM (
                    SELECT id, name, image_file_id, name_sort, similarity(name_sort, $1) AS sim
                    FROM furumusic__artist WHERE is_hidden = false AND name_sort % $1
                    UNION ALL
                    SELECT id, name, image_file_id, name_sort, 0.01::real AS sim
                    FROM furumusic__artist WHERE is_hidden = false AND name_sort ILIKE '%' || $1 || '%'
                ) a
                GROUP BY a.id, a.name, a.image_file_id
                ORDER BY similarity DESC
                LIMIT $2
            ) sub"#,
        )
        .bind(&q)
        .bind(limit)
        .fetch_all(pool);

        let r = sqlx::query_as::<_, SearchReleaseRow>(
            r#"SELECT id, title, release_type, year, cover_file_id, track_count FROM (
                SELECT r.id, r.title::text AS title, r.release_type::text AS release_type,
                       r.year, r.cover_file_id,
                       COALESCE((SELECT COUNT(*) FROM furumusic__track t WHERE t.release_id = r.id AND t.is_hidden = false), 0) AS track_count,
                       MAX(sim) AS similarity
                FROM (
                    SELECT id, title, release_type, year, cover_file_id, title_sort, similarity(title_sort, $1) AS sim
                    FROM furumusic__release WHERE is_hidden = false AND title_sort % $1
                    UNION ALL
                    SELECT id, title, release_type, year, cover_file_id, title_sort, 0.01::real AS sim
                    FROM furumusic__release WHERE is_hidden = false AND title_sort ILIKE '%' || $1 || '%'
                ) r
                GROUP BY r.id, r.title, r.release_type, r.year, r.cover_file_id
                ORDER BY similarity DESC
                LIMIT $2
            ) sub"#,
        )
        .bind(&q)
        .bind(limit)
        .fetch_all(pool);

        let t = sqlx::query_as::<_, SearchTrackRow>(
            r#"SELECT id, title, track_number, disc_number, duration_seconds, cover_file_id, release_cover_file_id FROM (
                SELECT t.id, t.title::text AS title, t.track_number, t.disc_number,
                       t.duration_seconds, t.cover_file_id,
                       rel.cover_file_id AS release_cover_file_id,
                       MAX(sim) AS similarity
                FROM (
                    SELECT id, title, title_sort, track_number, disc_number, duration_seconds, cover_file_id, release_id,
                           similarity(title_sort, $1) AS sim
                    FROM furumusic__track WHERE is_hidden = false AND title_sort % $1
                    UNION ALL
                    SELECT id, title, title_sort, track_number, disc_number, duration_seconds, cover_file_id, release_id,
                           0.01::real AS sim
                    FROM furumusic__track WHERE is_hidden = false AND title_sort ILIKE '%' || $1 || '%'
                ) t
                JOIN furumusic__release rel ON rel.id = t.release_id
                GROUP BY t.id, t.title, t.track_number, t.disc_number, t.duration_seconds, t.cover_file_id, rel.cover_file_id
                ORDER BY similarity DESC
                LIMIT $2
            ) sub"#,
        )
        .bind(&q)
        .bind(limit)
        .fetch_all(pool);

        tokio::try_join!(a, r, t).map_err(|e| cot::Error::internal(e.to_string()))?
    };

    // Collect track IDs for batch artist lookup
    let track_ids: Vec<i64> = track_rows.iter().map(|t| t.id).collect();

    let track_artists = if track_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TrackArtistRow>(
            r#"SELECT ta.track_id, ta.artist_id, a.name::text AS artist_name, ta.role::text AS role
               FROM furumusic__track_artist ta
               JOIN furumusic__artist a ON a.id = ta.artist_id
               WHERE ta.track_id = ANY($1)
               ORDER BY ta.track_id, ta.position"#,
        )
        .bind(&track_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?
    };

    let mut track_main_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();
    let mut track_feat_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();

    for ta in &track_artists {
        let artist_ref = ArtistRef {
            id: ta.artist_id,
            name: ta.artist_name.clone(),
        };
        if ta.role == "featuring" {
            track_feat_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        } else {
            track_main_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        }
    }

    let artists: Vec<ArtistCard> = artist_rows
        .into_iter()
        .map(|r| ArtistCard {
            id: r.id,
            name: r.name,
            image_url: cover_url(r.image_file_id),
            release_count: r.release_count,
            track_count: r.track_count,
        })
        .collect();

    let releases: Vec<ReleaseCard> = release_rows
        .into_iter()
        .map(|r| ReleaseCard {
            id: r.id,
            title: r.title,
            release_type: r.release_type,
            year: r.year,
            cover_url: cover_url(r.cover_file_id),
            track_count: r.track_count,
        })
        .collect();

    let tracks: Vec<TrackItem> = track_rows
        .into_iter()
        .map(|t| {
            let tid = t.id;
            TrackItem {
                id: t.id,
                title: t.title,
                track_number: t.track_number,
                disc_number: t.disc_number,
                duration_seconds: t.duration_seconds,
                artists: track_main_artists.remove(&tid).unwrap_or_default(),
                featured_artists: track_feat_artists.remove(&tid).unwrap_or_default(),
                cover_url: track_cover_url(t.cover_file_id, t.release_cover_file_id),
                stream_url: format!("/api/player/stream/{tid}"),
            }
        })
        .collect();

    Json(SearchResults {
        artists,
        releases,
        tracks,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// POST /api/player/playlists  — create playlist
// ---------------------------------------------------------------------------

async fn create_playlist_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    Json(body): Json<CreatePlaylistRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return Ok(json_error(StatusCode::BAD_REQUEST, "title is required"));
    }
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO furumusic__playlist (owner_id, title, is_public, created_at, updated_at) \
         VALUES ($1, $2, false, $3, $3) RETURNING id",
    )
    .bind(user.id)
    .bind(&title)
    .bind(&now)
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(PlaylistCard {
        id: row.0,
        title,
        track_count: 0,
        is_own: true,
        kind: "user".to_string(),
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// PUT /api/player/playlists/{id}  — rename / update playlist
// ---------------------------------------------------------------------------

async fn update_playlist_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
    Json(body): Json<UpdatePlaylistRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let playlist_id = path.0.id;
    // Verify ownership
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT owner_id FROM furumusic__playlist WHERE id = $1")
            .bind(playlist_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;
    let Some(owner) = owner else {
        return Ok(json_error(StatusCode::NOT_FOUND, "playlist not found"));
    };
    if owner.0 != user.id {
        return Ok(json_error(StatusCode::FORBIDDEN, "not your playlist"));
    }
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    if let Some(title) = &body.title {
        let t = title.trim();
        if !t.is_empty() {
            sqlx::query("UPDATE furumusic__playlist SET title = $1, updated_at = $2 WHERE id = $3")
                .bind(t)
                .bind(&now)
                .bind(playlist_id)
                .execute(pool)
                .await
                .map_err(|e| cot::Error::internal(e.to_string()))?;
        }
    }
    if let Some(desc) = &body.description {
        sqlx::query(
            "UPDATE furumusic__playlist SET description = $1, updated_at = $2 WHERE id = $3",
        )
        .bind(desc)
        .bind(&now)
        .bind(playlist_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    }
    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// DELETE /api/player/playlists/{id}
// ---------------------------------------------------------------------------

async fn delete_playlist_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let playlist_id = path.0.id;
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT owner_id FROM furumusic__playlist WHERE id = $1")
            .bind(playlist_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;
    let Some(owner) = owner else {
        return Ok(json_error(StatusCode::NOT_FOUND, "playlist not found"));
    };
    if owner.0 != user.id {
        return Ok(json_error(StatusCode::FORBIDDEN, "not your playlist"));
    }
    sqlx::query("DELETE FROM furumusic__playlist_track WHERE playlist_id = $1")
        .bind(playlist_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    sqlx::query("DELETE FROM furumusic__saved_playlist WHERE playlist_id = $1")
        .bind(playlist_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    sqlx::query("DELETE FROM furumusic__playlist WHERE id = $1")
        .bind(playlist_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// POST /api/player/playlists/{id}/tracks  — add tracks to playlist
// ---------------------------------------------------------------------------

async fn add_tracks_to_playlist_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
    Json(body): Json<AddTracksRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let playlist_id = path.0.id;
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT owner_id FROM furumusic__playlist WHERE id = $1")
            .bind(playlist_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;
    let Some(owner) = owner else {
        return Ok(json_error(StatusCode::NOT_FOUND, "playlist not found"));
    };
    if owner.0 != user.id {
        return Ok(json_error(StatusCode::FORBIDDEN, "not your playlist"));
    }

    // Get next position
    let max_pos: (Option<i32>,) = sqlx::query_as(
        "SELECT MAX(position) FROM furumusic__playlist_track WHERE playlist_id = $1",
    )
    .bind(playlist_id)
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let mut pos = max_pos.0.unwrap_or(-1) + 1;
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    for track_id in &body.track_ids {
        sqlx::query(
            "INSERT INTO furumusic__playlist_track (playlist_id, track_id, position, added_at, added_by_user_id) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(pos)
        .bind(&now)
        .bind(user.id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
        pos += 1;
    }

    sqlx::query("UPDATE furumusic__playlist SET updated_at = $1 WHERE id = $2")
        .bind(&now)
        .bind(playlist_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// DELETE /api/player/playlists/{id}/tracks  — remove a track from playlist
// ---------------------------------------------------------------------------

async fn remove_track_from_playlist_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
    Json(body): Json<RemoveTrackRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let playlist_id = path.0.id;
    let owner: Option<(i64,)> =
        sqlx::query_as("SELECT owner_id FROM furumusic__playlist WHERE id = $1")
            .bind(playlist_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;
    let Some(owner) = owner else {
        return Ok(json_error(StatusCode::NOT_FOUND, "playlist not found"));
    };
    if owner.0 != user.id {
        return Ok(json_error(StatusCode::FORBIDDEN, "not your playlist"));
    }

    sqlx::query("DELETE FROM furumusic__playlist_track WHERE playlist_id = $1 AND track_id = $2")
        .bind(playlist_id)
        .bind(body.track_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    // Re-number positions
    sqlx::query(
        r#"WITH ordered AS (
             SELECT id, ROW_NUMBER() OVER (ORDER BY position) - 1 as new_pos
             FROM furumusic__playlist_track WHERE playlist_id = $1
           )
           UPDATE furumusic__playlist_track pt
           SET position = o.new_pos
           FROM ordered o WHERE pt.id = o.id"#,
    )
    .bind(playlist_id)
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// POST /api/player/likes/toggle/{track_id}  — toggle like on a track
// ---------------------------------------------------------------------------

async fn toggle_like_track_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathTrackId>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let track_id = path.0.track_id;
    let existing: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM furumusic__user_liked_track WHERE user_id = $1 AND track_id = $2",
    )
    .bind(user.id)
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    if existing.is_some() {
        sqlx::query("DELETE FROM furumusic__user_liked_track WHERE user_id = $1 AND track_id = $2")
            .bind(user.id)
            .bind(track_id)
            .execute(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;
        Json(LikeStatus { liked: false }).into_response()
    } else {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        sqlx::query(
            "INSERT INTO furumusic__user_liked_track (user_id, track_id, created_at) VALUES ($1, $2, $3)",
        )
        .bind(user.id)
        .bind(track_id)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
        Json(LikeStatus { liked: true }).into_response()
    }
}

// ---------------------------------------------------------------------------
// POST /api/player/likes/release/{release_id}  — like all tracks in release
// ---------------------------------------------------------------------------

async fn like_release_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let release_id = path.0.id;
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Check if ALL tracks in this release are already liked
    let total: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM furumusic__track WHERE release_id = $1 AND is_hidden = false",
    )
    .bind(release_id)
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let liked_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM furumusic__user_liked_track ult
           JOIN furumusic__track t ON t.id = ult.track_id
           WHERE ult.user_id = $1 AND t.release_id = $2 AND t.is_hidden = false"#,
    )
    .bind(user.id)
    .bind(release_id)
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    if liked_count.0 >= total.0 && total.0 > 0 {
        // Unlike all tracks in release
        sqlx::query(
            r#"DELETE FROM furumusic__user_liked_track
               WHERE user_id = $1 AND track_id IN (
                   SELECT id FROM furumusic__track WHERE release_id = $2 AND is_hidden = false
               )"#,
        )
        .bind(user.id)
        .bind(release_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
        Json(LikeStatus { liked: false }).into_response()
    } else {
        // Like all tracks in release (skip already liked)
        sqlx::query(
            r#"INSERT INTO furumusic__user_liked_track (user_id, track_id, created_at)
               SELECT $1, t.id, $3
               FROM furumusic__track t
               WHERE t.release_id = $2 AND t.is_hidden = false
                 AND NOT EXISTS (
                     SELECT 1 FROM furumusic__user_liked_track ult
                     WHERE ult.user_id = $1 AND ult.track_id = t.id
                 )"#,
        )
        .bind(user.id)
        .bind(release_id)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
        Json(LikeStatus { liked: true }).into_response()
    }
}

// ---------------------------------------------------------------------------
// GET /api/player/likes  — get all liked track IDs for current user
// ---------------------------------------------------------------------------

async fn liked_ids_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT track_id FROM furumusic__user_liked_track WHERE user_id = $1")
            .bind(user.id)
            .fetch_all(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(LikedIds {
        track_ids: rows.into_iter().map(|r| r.0).collect(),
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// POST /api/player/tracks-by-ids
// ---------------------------------------------------------------------------

async fn tracks_by_ids_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    Json(body): Json<TracksByIdsRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    if body.ids.is_empty() {
        return Json(Vec::<TrackItem>::new()).into_response();
    }

    // Limit to 500 IDs to prevent abuse
    let ids: Vec<i64> = body.ids.into_iter().take(500).collect();

    let tracks = sqlx::query_as::<_, TrackRow>(
        r#"SELECT t.id, t.title::text as title, t.track_number, t.disc_number,
                  t.duration_seconds, t.cover_file_id,
                  r.cover_file_id as release_cover_file_id
           FROM furumusic__track t
           JOIN furumusic__release r ON r.id = t.release_id
           WHERE t.id = ANY($1) AND t.is_hidden = false"#,
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let track_ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();

    let track_artists = if track_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TrackArtistRow>(
            r#"SELECT ta.track_id, ta.artist_id, a.name::text as artist_name, ta.role::text as role
               FROM furumusic__track_artist ta
               JOIN furumusic__artist a ON a.id = ta.artist_id
               WHERE ta.track_id = ANY($1)
               ORDER BY ta.track_id, ta.position"#,
        )
        .bind(&track_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?
    };

    let mut track_main_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();
    let mut track_feat_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();

    for ta in &track_artists {
        let artist_ref = ArtistRef {
            id: ta.artist_id,
            name: ta.artist_name.clone(),
        };
        if ta.role == "featuring" {
            track_feat_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        } else {
            track_main_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        }
    }

    // Build a map from id -> TrackItem
    let mut track_map: std::collections::HashMap<i64, TrackItem> = std::collections::HashMap::new();
    for t in tracks {
        let tid = t.id;
        track_map.insert(
            tid,
            TrackItem {
                id: t.id,
                title: t.title,
                track_number: t.track_number,
                disc_number: t.disc_number,
                duration_seconds: t.duration_seconds,
                artists: track_main_artists.remove(&tid).unwrap_or_default(),
                featured_artists: track_feat_artists.remove(&tid).unwrap_or_default(),
                cover_url: track_cover_url(t.cover_file_id, t.release_cover_file_id),
                stream_url: format!("/api/player/stream/{tid}"),
            },
        );
    }

    // Reorder results to match input order
    let result: Vec<TrackItem> = ids.iter().filter_map(|id| track_map.remove(id)).collect();

    Json(result).into_response()
}

// ---------------------------------------------------------------------------
// PlayerApp
// ---------------------------------------------------------------------------

pub struct PlayerApp {
    config: Arc<AppConfig>,
    scheduler_handle: Arc<tokio::sync::OnceCell<Arc<SchedulerHandle>>>,
}

impl PlayerApp {
    pub fn new(
        config: Arc<AppConfig>,
        scheduler_handle: Arc<tokio::sync::OnceCell<Arc<SchedulerHandle>>>,
    ) -> Self {
        Self {
            config,
            scheduler_handle,
        }
    }
}

impl App for PlayerApp {
    fn name(&self) -> &'static str {
        "player"
    }

    fn router(&self) -> Router {
        let pool_config = Arc::clone(&self.config);
        let pool: Arc<tokio::sync::OnceCell<sqlx::PgPool>> = Arc::new(tokio::sync::OnceCell::new());
        let torrent_service: Arc<tokio::sync::OnceCell<Arc<TorrentService>>> =
            Arc::new(tokio::sync::OnceCell::new());

        Router::with_urls([
            // -- Current user profile --
            Route::with_handler_and_name(
                "/me",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(move |session: Session, db: Database| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            me_handler(session, db, pg_pool).await
                        }
                    })
                },
                "player_me",
            ),
            // -- Torrent import widget --
            Route::with_handler_and_name(
                "/torrents/preview",
                {
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    post(
                        move |session: Session, db: Database, json: Json<TorrentPreviewRequest>| {
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(_user) = auth::get_session_user(&session, &db).await
                                else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.preview(json.0).await {
                                    Ok(preview) => Json(preview).into_response(),
                                    Err(err) => {
                                        Ok(json_error(StatusCode::BAD_REQUEST, &err.to_string()))
                                    }
                                }
                            }
                        },
                    )
                },
                "player_torrent_preview",
            ),
            Route::with_handler_and_name(
                "/torrents/{id}/start",
                {
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    post(
                        move |session: Session,
                              db: Database,
                              path: Path<PathStringId>,
                              json: Json<TorrentStartRequest>| {
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(_user) = auth::get_session_user(&session, &db).await
                                else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let (live_config, _) = AppConfig::load_with_db(&db).await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service
                                    .start(
                                        &path.0.id,
                                        json.0.selected_files,
                                        live_config.agent_inbox_dir,
                                    )
                                    .await
                                {
                                    Ok(job) => Json(job).into_response(),
                                    Err(err) => {
                                        Ok(json_error(StatusCode::BAD_REQUEST, &err.to_string()))
                                    }
                                }
                            }
                        },
                    )
                },
                "player_torrent_start",
            ),
            Route::with_handler_and_name(
                "/torrents/{id}/status",
                {
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    get(
                        move |session: Session, db: Database, path: Path<PathStringId>| {
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(_user) = auth::get_session_user(&session, &db).await
                                else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.status(&path.0.id).await {
                                    Ok(job) => Json(job).into_response(),
                                    Err(err) => {
                                        Ok(json_error(StatusCode::NOT_FOUND, &err.to_string()))
                                    }
                                }
                            }
                        },
                    )
                },
                "player_torrent_status",
            ),
            // -- Artists (paginated) --
            Route::with_handler_and_name(
                "/artists",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(move |session: Session, db: Database,
                              query: cot::request::extractors::UrlQuery<PaginationQuery>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            artists_handler(session, db, pg_pool, query).await
                        }
                    })
                },
                "player_artists",
            ),
            // -- Artist detail --
            Route::with_handler_and_name(
                "/artists/{id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(move |session: Session, db: Database, path: Path<PathId>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            artist_detail_handler(session, db, pg_pool, path).await
                        }
                    })
                },
                "player_artist_detail",
            ),
            // -- Release detail --
            Route::with_handler_and_name(
                "/releases/{id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(move |session: Session, db: Database, path: Path<PathId>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            release_detail_handler(session, db, pg_pool, path).await
                        }
                    })
                },
                "player_release_detail",
            ),
            // -- Playlists (list + create) --
            Route::with_handler_and_name(
                "/playlists",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            playlists_handler(session, db, pg_pool).await
                        }
                    }
                })
                .post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<CreatePlaylistRequest>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            create_playlist_handler(session, db, pg_pool, json).await
                        }
                    }
                }),
                "player_playlists",
            ),
            // -- Playlist detail (get, update, delete) --
            Route::with_handler_and_name(
                "/playlists/{id}",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, path: Path<PathId>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            playlist_detail_handler(session, db, pg_pool, path).await
                        }
                    }
                })
                .put({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session,
                          db: Database,
                          path: Path<PathId>,
                          json: Json<UpdatePlaylistRequest>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            update_playlist_handler(session, db, pg_pool, path, json).await
                        }
                    }
                })
                .delete({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, path: Path<PathId>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            delete_playlist_handler(session, db, pg_pool, path).await
                        }
                    }
                }),
                "player_playlist_detail",
            ),
            // -- Playlist tracks (add / remove) --
            Route::with_handler_and_name(
                "/playlists/{id}/tracks",
                post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session,
                          db: Database,
                          path: Path<PathId>,
                          json: Json<AddTracksRequest>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            add_tracks_to_playlist_handler(session, db, pg_pool, path, json).await
                        }
                    }
                })
                .delete({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session,
                          db: Database,
                          path: Path<PathId>,
                          json: Json<RemoveTrackRequest>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            remove_track_from_playlist_handler(session, db, pg_pool, path, json)
                                .await
                        }
                    }
                }),
                "player_playlist_tracks",
            ),
            // -- Likes (get liked IDs) --
            Route::with_handler_and_name(
                "/likes",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            liked_ids_handler(session, db, pg_pool).await
                        }
                    }
                }),
                "player_likes",
            ),
            // -- Toggle like on track --
            Route::with_handler_and_name(
                "/likes/toggle/{track_id}",
                post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, path: Path<PathTrackId>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            toggle_like_track_handler(session, db, pg_pool, path).await
                        }
                    }
                }),
                "player_like_toggle",
            ),
            // -- Like/unlike release --
            Route::with_handler_and_name(
                "/likes/release/{id}",
                post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, path: Path<PathId>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            like_release_handler(session, db, pg_pool, path).await
                        }
                    }
                }),
                "player_like_release",
            ),
            // -- Audio stream --
            Route::with_handler_and_name(
                "/stream/{track_id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let config = Arc::clone(&self.config);
                    get(
                        move |session: Session,
                              db: Database,
                              path: Path<PathTrackId>,
                              request: cot::request::Request| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let config = Arc::clone(&config);
                            async move {
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                stream_handler(session, db, pg_pool, &config, &request, path).await
                            }
                        },
                    )
                },
                "player_stream",
            ),
            // -- Cover art --
            Route::with_handler_and_name(
                "/cover/{media_file_id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let config = Arc::clone(&self.config);
                    get(
                        move |session: Session, db: Database, path: Path<PathMediaFileId>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let config = Arc::clone(&config);
                            async move {
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                cover_handler(session, db, pg_pool, &config, path).await
                            }
                        },
                    )
                },
                "player_cover",
            ),
            // -- Playback state GET --
            Route::with_handler_and_name(
                "/state",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            get_state_handler(session, db, pg_pool).await
                        }
                    }
                })
                .put({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<PlaybackStateDto>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            put_state_handler(session, db, pg_pool, json).await
                        }
                    }
                })
                .post({
                    // POST handler for sendBeacon (used on page unload)
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<PlaybackStateDto>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            put_state_handler(session, db, pg_pool, json).await
                        }
                    }
                }),
                "player_state",
            ),
            // -- Play history --
            Route::with_handler_and_name(
                "/history",
                cot::router::method::post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<HistoryEntry>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            history_handler(session, db, pg_pool, json).await
                        }
                    }
                }),
                "player_history",
            ),
            // -- Search --
            Route::with_handler_and_name(
                "/search",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database,
                          query: cot::request::extractors::UrlQuery<SearchQuery>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            search_handler(session, db, pg_pool, query).await
                        }
                    }
                }),
                "player_search",
            ),
            // -- Tracks by IDs --
            Route::with_handler_and_name(
                "/tracks-by-ids",
                post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<TracksByIdsRequest>| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        async move {
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            tracks_by_ids_handler(session, db, pg_pool, json).await
                        }
                    }
                }),
                "player_tracks_by_ids",
            ),
        ])
    }
}
