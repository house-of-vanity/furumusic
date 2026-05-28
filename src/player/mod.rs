use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use cot::db::Database;
use cot::http::StatusCode;
use cot::http::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderName, RANGE,
};
use cot::json::Json;
use cot::request::extractors::Path;
use cot::response::IntoResponse;
use cot::router::method::{get, post};
use cot::router::{Route, Router};
use cot::session::Session;
use cot::{App, Body, Template};

use crate::auth;
use crate::config::AppConfig;
use crate::i18n::Translations;
use crate::lastfm::{LastfmClient, LastfmCredentials, LastfmTrackPayload};
use crate::scheduler::SchedulerHandle;
use crate::torrents::{TorrentPreviewRequest, TorrentService, TorrentStartRequest};

mod dto;
mod helpers;
mod queries;
mod rows;

use dto::*;
use helpers::{cover_variant_url, load_release_uploaders, track_cover_variant_url};
use queries::*;
use rows::*;

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

#[derive(serde::Serialize)]
struct LocalUploadResponse {
    ok: bool,
    filename: String,
    size: u64,
}

const PLAYER_DEVICE_TTL_MS: i64 = 30_000;
const PLAYER_DEVICE_COMMAND_TTL_MS: i64 = 20_000;
const PLAYER_DEVICE_MAX_COMMANDS: usize = 32;

#[derive(Debug, Clone)]
struct PlayerDevice {
    id: String,
    name: String,
    kind: String,
    last_seen_ms: i64,
}

#[derive(Debug, Clone)]
struct PendingPlayerDeviceCommand {
    id: String,
    command: String,
    payload: serde_json::Value,
    created_at_ms: i64,
}

#[derive(Debug, Default)]
struct PlayerDeviceHubState {
    devices_by_user: HashMap<i64, HashMap<String, PlayerDevice>>,
    active_device_by_user: HashMap<i64, String>,
    commands_by_device: HashMap<(i64, String), VecDeque<PendingPlayerDeviceCommand>>,
    playback_state_by_user: HashMap<i64, PlayerDevicePlaybackStateDto>,
}

#[derive(Debug, Default)]
struct PlayerDeviceHub {
    state: Mutex<PlayerDeviceHubState>,
}

impl PlayerDeviceHub {
    fn heartbeat(
        &self,
        user_id: i64,
        device_id: &str,
        user_agent: Option<&str>,
        playback_state: Option<PlayerDevicePlaybackStateDto>,
    ) -> PlayerDevicesResponse {
        let now = current_millis();
        let mut state = self.state.lock().expect("player device hub lock");
        self.prune_locked(&mut state, now);
        self.touch_locked(&mut state, user_id, device_id, user_agent, now);
        self.update_playback_state_locked(&mut state, user_id, device_id, playback_state, now);
        self.snapshot_locked(&state, user_id, device_id, now)
    }

    fn poll(
        &self,
        user_id: i64,
        device_id: &str,
        user_agent: Option<&str>,
        playback_state: Option<PlayerDevicePlaybackStateDto>,
    ) -> PlayerDevicePollResponse {
        let now = current_millis();
        let mut state = self.state.lock().expect("player device hub lock");
        self.prune_locked(&mut state, now);
        self.touch_locked(&mut state, user_id, device_id, user_agent, now);
        self.update_playback_state_locked(&mut state, user_id, device_id, playback_state, now);
        let commands = state
            .commands_by_device
            .remove(&(user_id, device_id.to_string()))
            .unwrap_or_default()
            .into_iter()
            .map(|cmd| PlayerDeviceCommandDto {
                id: cmd.id,
                command: cmd.command,
                payload: cmd.payload,
            })
            .collect();
        let snapshot = self.snapshot_locked(&state, user_id, device_id, now);
        PlayerDevicePollResponse {
            device_id: snapshot.device_id,
            active_device_id: snapshot.active_device_id,
            devices: snapshot.devices,
            commands,
            playback_state: snapshot.playback_state,
        }
    }

    fn select(
        &self,
        user_id: i64,
        current_device_id: &str,
        target_device_id: &str,
    ) -> Option<PlayerDevicesResponse> {
        let now = current_millis();
        let mut state = self.state.lock().expect("player device hub lock");
        self.prune_locked(&mut state, now);
        let devices = state.devices_by_user.get(&user_id)?;
        if !devices.contains_key(target_device_id) {
            return None;
        }
        let previous_active_id = state.active_device_by_user.get(&user_id).cloned();
        let transfer_state = state
            .playback_state_by_user
            .get(&user_id)
            .cloned()
            .map(|playback_state| playback_state_at(playback_state, now));
        state
            .active_device_by_user
            .insert(user_id, target_device_id.to_string());
        if previous_active_id.as_deref() != Some(target_device_id) {
            if let Some(transfer_state) = transfer_state {
                state
                    .playback_state_by_user
                    .insert(user_id, transfer_state.clone());
                if let Ok(payload) = serde_json::to_value(transfer_state) {
                    self.enqueue_command_locked(
                        &mut state,
                        user_id,
                        target_device_id,
                        "transfer_state",
                        payload,
                        now,
                    );
                }
            }
        }
        Some(self.snapshot_locked(&state, user_id, current_device_id, now))
    }

    fn enqueue_command(
        &self,
        user_id: i64,
        target_device_id: Option<&str>,
        command: &str,
        payload: serde_json::Value,
    ) -> Result<(), &'static str> {
        let now = current_millis();
        let mut state = self.state.lock().expect("player device hub lock");
        self.prune_locked(&mut state, now);

        let target_id = match target_device_id {
            Some(id) => id.to_string(),
            None => state
                .active_device_by_user
                .get(&user_id)
                .cloned()
                .ok_or("no active device")?,
        };

        let devices = state
            .devices_by_user
            .get(&user_id)
            .ok_or("target device is offline")?;
        if !devices.contains_key(&target_id) {
            return Err("target device is offline");
        }

        self.enqueue_command_locked(&mut state, user_id, &target_id, command, payload, now);
        Ok(())
    }

    fn enqueue_command_locked(
        &self,
        state: &mut PlayerDeviceHubState,
        user_id: i64,
        target_device_id: &str,
        command: &str,
        payload: serde_json::Value,
        now: i64,
    ) {
        let queue = state
            .commands_by_device
            .entry((user_id, target_device_id.to_string()))
            .or_default();
        while queue.len() >= PLAYER_DEVICE_MAX_COMMANDS {
            queue.pop_front();
        }
        queue.push_back(PendingPlayerDeviceCommand {
            id: uuid::Uuid::new_v4().simple().to_string(),
            command: command.to_string(),
            payload,
            created_at_ms: now,
        });
    }

    fn touch_locked(
        &self,
        state: &mut PlayerDeviceHubState,
        user_id: i64,
        device_id: &str,
        user_agent: Option<&str>,
        now: i64,
    ) {
        let devices = state.devices_by_user.entry(user_id).or_default();
        let device = PlayerDevice {
            id: device_id.to_string(),
            name: device_name_from_user_agent(user_agent),
            kind: device_kind_from_user_agent(user_agent).to_string(),
            last_seen_ms: now,
        };
        devices.insert(device_id.to_string(), device);

        let active_online = state
            .active_device_by_user
            .get(&user_id)
            .is_some_and(|active_id| devices.contains_key(active_id));
        if !active_online {
            state
                .active_device_by_user
                .insert(user_id, device_id.to_string());
        }
    }

    fn update_playback_state_locked(
        &self,
        state: &mut PlayerDeviceHubState,
        user_id: i64,
        device_id: &str,
        playback_state: Option<PlayerDevicePlaybackStateDto>,
        now: i64,
    ) {
        let is_active = state
            .active_device_by_user
            .get(&user_id)
            .is_some_and(|active_id| active_id == device_id);
        if !is_active {
            return;
        }
        let Some(mut playback_state) = playback_state else {
            return;
        };
        playback_state.updated_at_ms = now;
        state.playback_state_by_user.insert(user_id, playback_state);
    }

    fn snapshot_locked(
        &self,
        state: &PlayerDeviceHubState,
        user_id: i64,
        current_device_id: &str,
        now: i64,
    ) -> PlayerDevicesResponse {
        let active_device_id = state.active_device_by_user.get(&user_id).cloned();
        let mut devices: Vec<PlayerDeviceDto> = state
            .devices_by_user
            .get(&user_id)
            .map(|devices| {
                devices
                    .values()
                    .map(|device| PlayerDeviceDto {
                        id: device.id.clone(),
                        name: device.name.clone(),
                        kind: device.kind.clone(),
                        is_current: device.id == current_device_id,
                        is_active: active_device_id.as_deref() == Some(device.id.as_str()),
                        last_seen_ms: now.saturating_sub(device.last_seen_ms),
                    })
                    .collect()
            })
            .unwrap_or_default();
        devices.sort_by(|a, b| {
            b.is_active
                .cmp(&a.is_active)
                .then_with(|| b.is_current.cmp(&a.is_current))
                .then_with(|| a.name.cmp(&b.name))
        });
        PlayerDevicesResponse {
            device_id: current_device_id.to_string(),
            active_device_id,
            devices,
            playback_state: state.playback_state_by_user.get(&user_id).cloned(),
        }
    }

    fn prune_locked(&self, state: &mut PlayerDeviceHubState, now: i64) {
        state.devices_by_user.retain(|user_id, devices| {
            devices.retain(|_, device| {
                now.saturating_sub(device.last_seen_ms) <= PLAYER_DEVICE_TTL_MS
            });
            let active_valid = state
                .active_device_by_user
                .get(user_id)
                .is_some_and(|active_id| devices.contains_key(active_id));
            if !active_valid {
                if let Some(first_device_id) = devices.keys().next().cloned() {
                    state
                        .active_device_by_user
                        .insert(*user_id, first_device_id);
                } else {
                    state.active_device_by_user.remove(user_id);
                    state.playback_state_by_user.remove(user_id);
                }
            }
            !devices.is_empty()
        });
        state
            .playback_state_by_user
            .retain(|user_id, _| state.devices_by_user.contains_key(user_id));

        state
            .commands_by_device
            .retain(|(user_id, device_id), queue| {
                let device_online = state
                    .devices_by_user
                    .get(user_id)
                    .is_some_and(|devices| devices.contains_key(device_id));
                if !device_online {
                    return false;
                }
                queue.retain(|cmd| {
                    now.saturating_sub(cmd.created_at_ms) <= PLAYER_DEVICE_COMMAND_TTL_MS
                });
                !queue.is_empty()
            });
    }
}

fn current_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn playback_state_at(
    mut playback_state: PlayerDevicePlaybackStateDto,
    now: i64,
) -> PlayerDevicePlaybackStateDto {
    if !playback_state.paused && playback_state.updated_at_ms > 0 {
        let elapsed_seconds = now.saturating_sub(playback_state.updated_at_ms) as f64 / 1000.0;
        playback_state.position_seconds += elapsed_seconds;
        if playback_state.duration_seconds > 0.0 {
            playback_state.position_seconds = playback_state
                .position_seconds
                .min(playback_state.duration_seconds);
        }
    }
    playback_state.updated_at_ms = now;
    playback_state
}

fn normalize_device_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn device_name_from_user_agent(user_agent: Option<&str>) -> String {
    let ua = user_agent.unwrap_or_default().to_ascii_lowercase();
    let browser = if ua.contains("edg/") || ua.contains("edgios/") || ua.contains("edga/") {
        "Edge"
    } else if ua.contains("firefox/") || ua.contains("fxios/") {
        "Firefox"
    } else if ua.contains("opr/") || ua.contains("opera") {
        "Opera"
    } else if ua.contains("chrome/") || ua.contains("crios/") {
        "Chrome"
    } else if ua.contains("safari/") {
        "Safari"
    } else {
        "Browser"
    };

    let os = if ua.contains("iphone") {
        "iPhone"
    } else if ua.contains("ipad") {
        "iPad"
    } else if ua.contains("android") {
        "Android"
    } else if ua.contains("windows") {
        "Windows"
    } else if ua.contains("mac os") || ua.contains("macintosh") {
        "macOS"
    } else if ua.contains("linux") {
        "Linux"
    } else {
        "Device"
    };

    format!("{browser} on {os}")
}

fn device_kind_from_user_agent(user_agent: Option<&str>) -> &'static str {
    let ua = user_agent.unwrap_or_default().to_ascii_lowercase();
    if ua.contains("iphone") || (ua.contains("android") && ua.contains("mobile")) {
        "phone"
    } else if ua.contains("ipad") || ua.contains("tablet") || ua.contains("android") {
        "tablet"
    } else {
        "computer"
    }
}

#[derive(Debug, sqlx::FromRow)]
struct LastfmAccountApiRow {
    session_key: String,
    reauth_required: bool,
    last_error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct LastfmStatusRow {
    username: String,
    reauth_required: bool,
    last_error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct LastfmTrackMetaRow {
    title: String,
    duration_seconds: f64,
    track_number: Option<i32>,
    album_title: Option<String>,
    artist_name: Option<String>,
    album_artist_name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct LastfmCallbackQuery {
    token: Option<String>,
    state: Option<String>,
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
// Last.fm account + scrobbling
// ---------------------------------------------------------------------------

fn redirect_response(location: &str) -> cot::response::Response {
    cot::http::Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(cot::http::header::LOCATION, location)
        .body(Body::fixed(""))
        .expect("valid response")
}

fn request_origin(request: &cot::request::Request) -> Option<String> {
    let headers = request.headers();
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))?
        .to_str()
        .ok()?;
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    Some(format!("{proto}://{host}"))
}

async fn lastfm_status_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let (config, _) = AppConfig::load_with_db(&db).await;
    let configured = crate::lastfm::is_configured(&config);
    let account = sqlx::query_as::<_, LastfmStatusRow>(
        r#"SELECT lastfm_username::text AS username,
                  reauth_required,
                  last_error::text AS last_error
             FROM furumusic__lastfm_account
            WHERE user_id = $1"#,
    )
    .bind(user.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(LastfmStatus {
        configured,
        connected: account.is_some(),
        username: account.as_ref().map(|row| row.username.clone()),
        reauth_required: account
            .as_ref()
            .map(|row| row.reauth_required)
            .unwrap_or(false),
        last_error: account.and_then(|row| row.last_error),
    })
    .into_response()
}

async fn lastfm_connect_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    request: cot::request::Request,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(redirect_response("/login"));
    };
    let (config, _) = AppConfig::load_with_db(&db).await;
    let Some(credentials) = LastfmCredentials::from_config(&config) else {
        return Ok(redirect_response("/"));
    };
    let Some(origin) = request_origin(&request) else {
        return Ok(redirect_response("/"));
    };

    let state = uuid::Uuid::new_v4().simple().to_string();
    let now = chrono::Utc::now();
    let stale = (now - chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    sqlx::query("DELETE FROM furumusic__lastfm_auth_state WHERE created_at < $1")
        .bind(stale)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    sqlx::query(
        r#"INSERT INTO furumusic__lastfm_auth_state (state, user_id, created_at)
           VALUES ($1, $2, $3)
           ON CONFLICT (state) DO NOTHING"#,
    )
    .bind(&state)
    .bind(user.id)
    .bind(now.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let callback = format!("{origin}/api/player/lastfm/callback?state={state}");
    let mut url = reqwest::Url::parse("https://www.last.fm/api/auth/")
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("api_key", credentials.api_key())
        .append_pair("cb", &callback);
    Ok(redirect_response(url.as_str()))
}

async fn lastfm_callback_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    query: cot::request::extractors::UrlQuery<LastfmCallbackQuery>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(redirect_response("/login"));
    };
    let Some(token) = query
        .0
        .token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(redirect_response("/"));
    };
    let Some(state) = query
        .0
        .state
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(redirect_response("/"));
    };

    let state_user_id = sqlx::query_scalar::<_, i64>(
        "SELECT user_id FROM furumusic__lastfm_auth_state WHERE state = $1",
    )
    .bind(state)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;
    if state_user_id != Some(user.id) {
        return Ok(redirect_response("/"));
    }
    sqlx::query("DELETE FROM furumusic__lastfm_auth_state WHERE state = $1")
        .bind(state)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let (config, _) = AppConfig::load_with_db(&db).await;
    let Some(credentials) = LastfmCredentials::from_config(&config) else {
        return Ok(redirect_response("/"));
    };
    let client = LastfmClient::new(credentials).map_err(|e| cot::Error::internal(e.to_string()))?;
    match client.get_session(token).await {
        Ok(lastfm_session) => {
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            sqlx::query(
                r#"INSERT INTO furumusic__lastfm_account
                      (user_id, lastfm_username, session_key, connected_at, updated_at, last_error, reauth_required)
                   VALUES ($1, $2, $3, $4, $4, NULL, false)
                   ON CONFLICT (user_id) DO UPDATE SET
                      lastfm_username = EXCLUDED.lastfm_username,
                      session_key = EXCLUDED.session_key,
                      updated_at = EXCLUDED.updated_at,
                      last_error = NULL,
                      reauth_required = false"#,
            )
            .bind(user.id)
            .bind(&lastfm_session.username)
            .bind(&lastfm_session.session_key)
            .bind(&now)
            .execute(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;
            Ok(redirect_response("/"))
        }
        Err(err) => {
            tracing::warn!("Last.fm auth failed for user {}: {err}", user.id);
            Ok(redirect_response("/"))
        }
    }
}

async fn lastfm_disconnect_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    sqlx::query("DELETE FROM furumusic__lastfm_account WHERE user_id = $1")
        .bind(user.id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
    sqlx::query(
        r#"UPDATE furumusic__lastfm_scrobble_outbox
              SET status = 'blocked',
                  last_error = 'Last.fm account disconnected',
                  updated_at = $2
            WHERE user_id = $1 AND status IN ('pending', 'retry')"#,
    )
    .bind(user.id)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;
    Json(serde_json::json!({"ok": true})).into_response()
}

async fn load_lastfm_account(
    pool: &sqlx::PgPool,
    user_id: i64,
) -> cot::Result<Option<LastfmAccountApiRow>> {
    sqlx::query_as::<_, LastfmAccountApiRow>(
        r#"SELECT session_key::text AS session_key,
                  reauth_required,
                  last_error::text AS last_error
             FROM furumusic__lastfm_account
            WHERE user_id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))
}

async fn load_lastfm_track_payload(
    pool: &sqlx::PgPool,
    track_id: i64,
) -> cot::Result<Option<LastfmTrackPayload>> {
    let row = sqlx::query_as::<_, LastfmTrackMetaRow>(
        r#"SELECT t.title::text AS title,
                  t.duration_seconds,
                  t.track_number,
                  r.title::text AS album_title,
                  (
                    SELECT a.name::text
                      FROM furumusic__track_artist ta
                      JOIN furumusic__artist a ON a.id = ta.artist_id
                     WHERE ta.track_id = t.id AND ta.role <> 'featuring'
                     ORDER BY ta.position
                     LIMIT 1
                  ) AS artist_name,
                  (
                    SELECT a.name::text
                      FROM furumusic__release_artist ra
                      JOIN furumusic__artist a ON a.id = ra.artist_id
                     WHERE ra.release_id = r.id
                     ORDER BY ra.position
                     LIMIT 1
                  ) AS album_artist_name
             FROM furumusic__track t
             LEFT JOIN furumusic__release r ON r.id = t.release_id
            WHERE t.id = $1 AND t.is_hidden = false"#,
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Ok(row.and_then(|row| {
        let artist = row
            .artist_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        Some(LastfmTrackPayload {
            artist,
            track: row.title,
            album: row
                .album_title
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            album_artist: row
                .album_artist_name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            track_number: row.track_number,
            duration_seconds: Some(row.duration_seconds.round() as i32),
        })
    }))
}

async fn update_lastfm_account_error(
    pool: &sqlx::PgPool,
    user_id: i64,
    error: &str,
    reauth_required: bool,
) -> cot::Result<()> {
    sqlx::query(
        r#"UPDATE furumusic__lastfm_account
              SET last_error = $2,
                  reauth_required = $3,
                  updated_at = $4
            WHERE user_id = $1"#,
    )
    .bind(user_id)
    .bind(error)
    .bind(reauth_required)
    .bind(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;
    Ok(())
}

async fn enqueue_lastfm_scrobble(
    pool: &sqlx::PgPool,
    config: &AppConfig,
    user_id: i64,
    track_id: i64,
    started_at: Option<i64>,
    listened_seconds: i32,
) -> cot::Result<LastfmActionResponse> {
    if !crate::lastfm::is_configured(config) {
        return Ok(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Last.fm is not configured".to_string()),
        });
    }
    if load_lastfm_account(pool, user_id).await?.is_none() {
        return Ok(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Last.fm account is not connected".to_string()),
        });
    }
    let Some(track) = load_lastfm_track_payload(pool, track_id).await? else {
        return Ok(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Track has no primary artist for Last.fm".to_string()),
        });
    };
    let duration_seconds = track.duration_seconds.unwrap_or(0).max(0);
    if duration_seconds <= 30 {
        return Ok(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Track is too short to scrobble".to_string()),
        });
    }
    let threshold = ((duration_seconds as f64 / 2.0).min(240.0)).ceil() as i32;
    let listened_seconds = listened_seconds.max(0);
    if listened_seconds < threshold {
        return Ok(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Scrobble threshold has not been reached".to_string()),
        });
    }

    let now_ts = chrono::Utc::now().timestamp();
    let started_at = started_at
        .unwrap_or(now_ts - listened_seconds as i64)
        .min(now_ts);
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let dedupe_key = format!("{user_id}:{track_id}:{started_at}");
    sqlx::query(
        r#"INSERT INTO furumusic__lastfm_scrobble_outbox
              (user_id, track_id, started_at, listened_seconds, duration_seconds, status, created_at, updated_at, dedupe_key)
           VALUES ($1, $2, $3, $4, $5, 'pending', $6, $6, $7)
           ON CONFLICT (dedupe_key) DO NOTHING"#,
    )
    .bind(user_id)
    .bind(track_id)
    .bind(started_at)
    .bind(listened_seconds)
    .bind(duration_seconds)
    .bind(&now)
    .bind(&dedupe_key)
    .execute(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let sent = match crate::lastfm::process_pending_scrobbles(pool, config, Some(user_id), 10).await
    {
        Ok(summary) => summary.sent > 0,
        Err(err) => {
            tracing::warn!("Last.fm immediate scrobble send failed: {err:#}");
            false
        }
    };

    Ok(LastfmActionResponse {
        ok: true,
        queued: true,
        sent,
        message: None,
    })
}

async fn lastfm_now_playing_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    Json(entry): Json<LastfmNowPlayingRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let (config, _) = AppConfig::load_with_db(&db).await;
    let Some(credentials) = LastfmCredentials::from_config(&config) else {
        return Json(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Last.fm is not configured".to_string()),
        })
        .into_response();
    };
    let Some(account) = load_lastfm_account(pool, user.id).await? else {
        return Json(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Last.fm account is not connected".to_string()),
        })
        .into_response();
    };
    if account.reauth_required {
        return Json(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: account.last_error,
        })
        .into_response();
    }
    let Some(track) = load_lastfm_track_payload(pool, entry.track_id).await? else {
        return Json(LastfmActionResponse {
            ok: false,
            queued: false,
            sent: false,
            message: Some("Track has no primary artist for Last.fm".to_string()),
        })
        .into_response();
    };
    let client = LastfmClient::new(credentials).map_err(|e| cot::Error::internal(e.to_string()))?;
    match client
        .update_now_playing(&account.session_key, &track)
        .await
    {
        Ok(()) => Json(LastfmActionResponse {
            ok: true,
            queued: false,
            sent: true,
            message: None,
        })
        .into_response(),
        Err(err) => {
            let reauth_required = err.is_invalid_session();
            update_lastfm_account_error(pool, user.id, &err.to_string(), reauth_required).await?;
            Json(LastfmActionResponse {
                ok: false,
                queued: false,
                sent: false,
                message: Some(err.to_string()),
            })
            .into_response()
        }
    }
}

async fn lastfm_scrobble_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    Json(entry): Json<LastfmScrobbleRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let (config, _) = AppConfig::load_with_db(&db).await;
    Json(
        enqueue_lastfm_scrobble(
            pool,
            &config,
            user.id,
            entry.track_id,
            entry.started_at,
            entry.listened_seconds,
        )
        .await?,
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/player/agent-queue
// ---------------------------------------------------------------------------

async fn agent_queue_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let (queued_count, processing_count): (i64, i64) = sqlx::query_as(
        r#"SELECT
              COUNT(*) FILTER (WHERE status = 'queued') AS queued_count,
              COUNT(*) FILTER (WHERE status = 'processing') AS processing_count
           FROM furumusic__pending_review"#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(AgentQueueStatus {
        queued_count,
        processing_count,
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
           WHERE a.is_hidden = false AND r.is_hidden = false AND ra.position = 0"#,
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
               WHERE ra.position = 0
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
            image_url: cover_variant_url(r.image_file_id, "medium"),
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

    let release_ids: Vec<i64> = releases.iter().map(|r| r.id).collect();
    let mut release_uploaders = load_release_uploaders(pool, &release_ids)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let release_cards: Vec<ReleaseCard> = releases
        .into_iter()
        .map(|r| ReleaseCard {
            id: r.id,
            title: r.title,
            release_type: r.release_type,
            year: r.year,
            cover_url: cover_variant_url(r.cover_file_id, "medium"),
            track_count: r.track_count,
            uploaders: release_uploaders.remove(&r.id).unwrap_or_default(),
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

    let featured_rows = sqlx::query_as::<_, AppearanceTrackRow>(
        r#"SELECT DISTINCT t.id,
                  t.title::text AS title,
                  r.id AS release_id,
                  r.title::text AS release_title,
                  r.year AS release_year,
                  t.duration_seconds,
                  t.cover_file_id,
                  r.cover_file_id AS release_cover_file_id,
                  COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                  mf.audio_format,
                  mf.audio_bitrate,
                  mf.audio_sample_rate,
                  mf.audio_bit_depth,
                  mf.file_size_bytes,
                  t.lastfm_listeners,
                  t.lastfm_playcount,
                  t.lastfm_rating,
                  t.lastfm_updated_at
           FROM furumusic__track_artist ta
           JOIN furumusic__track t ON t.id = ta.track_id
           JOIN furumusic__release r ON r.id = t.release_id
           LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
           WHERE ta.artist_id = $1
             AND ta.role = 'featuring'
             AND t.is_hidden = false
             AND r.is_hidden = false
           ORDER BY r.title::text, t.title::text"#,
    )
    .bind(artist_id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let featured_track_ids: Vec<i64> = featured_rows.iter().map(|t| t.id).collect();
    let featured_track_artists = if featured_track_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TrackArtistRow>(
            r#"SELECT ta.track_id, ta.artist_id, a.name::text as artist_name, ta.role::text as role
               FROM furumusic__track_artist ta
               JOIN furumusic__artist a ON a.id = ta.artist_id
               WHERE ta.track_id = ANY($1)
               ORDER BY ta.track_id, ta.position"#,
        )
        .bind(&featured_track_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?
    };

    let mut featured_main_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();
    let mut featured_feat_artists: std::collections::HashMap<i64, Vec<ArtistRef>> =
        std::collections::HashMap::new();

    for ta in &featured_track_artists {
        let artist_ref = ArtistRef {
            id: ta.artist_id,
            name: ta.artist_name.clone(),
        };
        if ta.role == "featuring" {
            featured_feat_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        } else {
            featured_main_artists
                .entry(ta.track_id)
                .or_default()
                .push(artist_ref);
        }
    }

    let featured_tracks: Vec<ArtistAppearanceTrack> = featured_rows
        .into_iter()
        .map(|t| {
            let tid = t.id;
            ArtistAppearanceTrack {
                id: t.id,
                title: t.title,
                release_id: t.release_id,
                release_title: t.release_title,
                release_year: t.release_year,
                duration_seconds: t.duration_seconds,
                artists: featured_main_artists.remove(&tid).unwrap_or_default(),
                featured_artists: featured_feat_artists.remove(&tid).unwrap_or_default(),
                cover_url: track_cover_variant_url(
                    t.cover_file_id,
                    t.release_cover_file_id,
                    "medium",
                ),
                stream_url: format!("/api/player/stream/{tid}"),
                uploader_name: t.uploader_name,
                audio_format: t.audio_format,
                audio_bitrate: t.audio_bitrate,
                audio_sample_rate: t.audio_sample_rate,
                audio_bit_depth: t.audio_bit_depth,
                file_size_bytes: t.file_size_bytes,
                lastfm_listeners: t.lastfm_listeners,
                lastfm_playcount: t.lastfm_playcount,
                lastfm_rating: t.lastfm_rating,
                lastfm_updated_at: t.lastfm_updated_at,
            }
        })
        .collect();

    Json(ArtistDetail {
        id: artist.id,
        name: artist.name,
        image_url: cover_variant_url(image_file_id, "large"),
        total_track_count,
        total_play_count,
        releases: release_cards,
        featured_tracks,
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
                  r.cover_file_id as release_cover_file_id,
                  r.id as release_id,
                  r.title::text as release_title,
                  r.year as release_year,
                  COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                  mf.audio_format,
                  mf.audio_bitrate,
                  mf.audio_sample_rate,
                  mf.audio_bit_depth,
                  mf.file_size_bytes,
                  t.lastfm_listeners,
                  t.lastfm_playcount,
                  t.lastfm_rating,
                  t.lastfm_updated_at
           FROM furumusic__track t
           JOIN furumusic__release r ON r.id = t.release_id
           LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
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
                release_id: t.release_id,
                release_title: t.release_title,
                release_year: t.release_year,
                cover_url: track_cover_variant_url(
                    t.cover_file_id,
                    t.release_cover_file_id,
                    "medium",
                ),
                stream_url: format!("/api/player/stream/{tid}"),
                uploader_name: t.uploader_name,
                audio_format: t.audio_format,
                audio_bitrate: t.audio_bitrate,
                audio_sample_rate: t.audio_sample_rate,
                audio_bit_depth: t.audio_bit_depth,
                file_size_bytes: t.file_size_bytes,
                lastfm_listeners: t.lastfm_listeners,
                lastfm_playcount: t.lastfm_playcount,
                lastfm_rating: t.lastfm_rating,
                lastfm_updated_at: t.lastfm_updated_at,
            }
        })
        .collect();
    let uploaders = load_release_uploaders(pool, &[release.id])
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?
        .remove(&release.id)
        .unwrap_or_default();

    Json(ReleaseDetail {
        id: release.id,
        title: release.title,
        release_type: release.release_type,
        year: release.year,
        cover_url: cover_variant_url(release.cover_file_id, "large"),
        artists: release_artists
            .into_iter()
            .map(|a| ArtistRef {
                id: a.id,
                name: a.name,
            })
            .collect(),
        tracks: track_items,
        uploaders,
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
        owner_name: None,
        is_public: false,
        is_saved: false,
        kind: "likes".to_string(),
    }];

    let rows = sqlx::query_as::<_, PlaylistRow>(
        r#"SELECT p.id, p.title::text as title,
                  COALESCE((SELECT COUNT(*) FROM furumusic__playlist_track pt WHERE pt.playlist_id = p.id), 0) as track_count,
                  (p.owner_id = $1) as is_own,
                  COALESCE(NULLIF(u.display_name, ''), u.username)::text as owner_name,
                  p.is_public,
                  EXISTS (
                      SELECT 1 FROM furumusic__saved_playlist sp
                      WHERE sp.user_id = $1 AND sp.playlist_id = p.id
                  ) as is_saved
           FROM furumusic__playlist p
           JOIN furumusic__user u ON u.id = p.owner_id
           WHERE p.owner_id = $1
              OR EXISTS (
                  SELECT 1 FROM furumusic__saved_playlist sp
                  WHERE sp.user_id = $1 AND sp.playlist_id = p.id
              )
              OR p.is_public = true
           ORDER BY
              CASE WHEN p.owner_id = $1 THEN 0 WHEN p.is_public THEN 2 ELSE 1 END,
              p.title"#,
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
        owner_name: Some(r.owner_name),
        is_public: r.is_public,
        is_saved: r.is_saved,
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
        r#"SELECT p.id, p.title::text as title, p.description, p.owner_id,
                  COALESCE(NULLIF(u.display_name, ''), u.username)::text as owner_name,
                  p.is_public,
                  EXISTS (
                      SELECT 1 FROM furumusic__saved_playlist sp
                      WHERE sp.user_id = $2 AND sp.playlist_id = p.id
                  ) as is_saved
           FROM furumusic__playlist p
           JOIN furumusic__user u ON u.id = p.owner_id
           WHERE p.id = $1"#,
    )
    .bind(playlist_id)
    .bind(user.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let Some(info) = info else {
        return Ok(json_error(StatusCode::NOT_FOUND, "playlist not found"));
    };

    let tracks = sqlx::query_as::<_, PlaylistTrackRow>(
        r#"SELECT t.id, t.title::text as title, t.track_number, t.disc_number,
                  t.duration_seconds, t.cover_file_id,
                  r.cover_file_id as release_cover_file_id,
                  r.id as release_id,
                  r.title::text as release_title,
                  r.year as release_year,
                  COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                  mf.audio_format,
                  mf.audio_bitrate,
                  mf.audio_sample_rate,
                  mf.audio_bit_depth,
                  mf.file_size_bytes,
                  t.lastfm_listeners,
                  t.lastfm_playcount,
                  t.lastfm_rating,
                  t.lastfm_updated_at
           FROM furumusic__playlist_track pt
           JOIN furumusic__track t ON t.id = pt.track_id
           JOIN furumusic__release r ON r.id = t.release_id
           LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
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
        owner_name: Some(info.owner_name),
        is_public: info.is_public,
        is_saved: info.is_saved,
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
                release_id: t.release_id,
                release_title: t.release_title,
                release_year: t.release_year,
                cover_url: track_cover_variant_url(
                    t.cover_file_id,
                    t.release_cover_file_id,
                    "medium",
                ),
                stream_url: format!("/api/player/stream/{tid}"),
                uploader_name: t.uploader_name,
                audio_format: t.audio_format,
                audio_bitrate: t.audio_bitrate,
                audio_sample_rate: t.audio_sample_rate,
                audio_bit_depth: t.audio_bit_depth,
                file_size_bytes: t.file_size_bytes,
                lastfm_listeners: t.lastfm_listeners,
                lastfm_playcount: t.lastfm_playcount,
                lastfm_rating: t.lastfm_rating,
                lastfm_updated_at: t.lastfm_updated_at,
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
                  r.cover_file_id as release_cover_file_id,
                  r.id as release_id,
                  r.title::text as release_title,
                  r.year as release_year,
                  COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                  mf.audio_format,
                  mf.audio_bitrate,
                  mf.audio_sample_rate,
                  mf.audio_bit_depth,
                  mf.file_size_bytes,
                  t.lastfm_listeners,
                  t.lastfm_playcount,
                  t.lastfm_rating,
                  t.lastfm_updated_at
           FROM furumusic__user_liked_track ult
           JOIN furumusic__track t ON t.id = ult.track_id
           JOIN furumusic__release r ON r.id = t.release_id
           LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
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
        owner_name: None,
        is_public: false,
        is_saved: false,
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

    let full_path =
        crate::media_paths::resolve_media_file_path(&config.agent_storage_dir, &media.file_path);

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

async fn local_upload_handler(
    session: Session,
    db: Database,
    config: AppConfig,
    scheduler_handle: Arc<tokio::sync::OnceCell<Arc<SchedulerHandle>>>,
    request: cot::request::Request,
) -> cot::Result<cot::http::Response<Body>> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let inbox_dir = config.agent_inbox_dir.trim();
    if inbox_dir.is_empty() {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            "agent_inbox_dir is not configured",
        ));
    }
    let inbox_root = crate::media_paths::resolve_config_path_buf(inbox_dir);
    if !inbox_root.is_absolute() {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            "agent_inbox_dir must be an absolute path",
        ));
    }

    let filename_header = HeaderName::from_static("x-furumusic-filename");
    let original_name = request
        .headers()
        .get(filename_header)
        .and_then(|value| value.to_str().ok())
        .map(percent_decode_header)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "upload.mp3".to_string());
    let filename = sanitize_upload_filename(&original_name);

    let bytes = request
        .into_body()
        .into_bytes()
        .await
        .map_err(|err| cot::Error::internal(err.to_string()))?;
    if bytes.is_empty() {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            "uploaded file is empty",
        ));
    }

    let upload_dir = inbox_root
        .join("user_uploads")
        .join(user.id.to_string())
        .join(format!("local-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|err| cot::Error::internal(err.to_string()))?;
    let destination = upload_dir.join(&filename);
    tokio::fs::write(&destination, &bytes)
        .await
        .map_err(|err| cot::Error::internal(err.to_string()))?;

    if let Some(handle) = scheduler_handle.get() {
        let handle = Arc::clone(handle);
        tokio::spawn(async move {
            if let Err(err) = handle.trigger_job_now("inbox_discover").await {
                tracing::warn!("failed to trigger inbox_discover after local upload: {err}");
            }
        });
    }

    Json(LocalUploadResponse {
        ok: true,
        filename,
        size: bytes.len() as u64,
    })
    .into_response()
}

fn sanitize_upload_filename(value: &str) -> String {
    let name = std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("upload.mp3");
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "upload.mp3".to_string()
    } else {
        trimmed.to_string()
    }
}

fn percent_decode_header(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hi = hex_value(bytes[index + 1]);
                let lo = hex_value(bytes[index + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi << 4) | lo);
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
    cover_response(session, db, pool, config, path.0.media_file_id, None).await
}

async fn cover_variant_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    config: &AppConfig,
    path: Path<PathMediaFileVariant>,
) -> cot::Result<cot::http::Response<Body>> {
    cover_response(
        session,
        db,
        pool,
        config,
        path.0.media_file_id,
        Some(path.0.variant.as_str()),
    )
    .await
}

async fn cover_response(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    config: &AppConfig,
    media_file_id: i64,
    variant_name: Option<&str>,
) -> cot::Result<cot::http::Response<Body>> {
    let Some(_user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

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

    let full_path =
        crate::media_paths::resolve_media_file_path(&config.agent_storage_dir, &media.file_path);

    if !full_path.exists() {
        return Ok(json_error(StatusCode::NOT_FOUND, "file not found on disk"));
    }

    let (response_path, content_type) = variant_name
        .and_then(crate::agent::cover_variants::variant_by_name)
        .map(|variant| {
            let variant_path = crate::agent::cover_variants::variant_path(&full_path, variant);
            if variant_path.exists() {
                (variant_path, "image/jpeg")
            } else {
                (full_path.clone(), media.mime_type.as_str())
            }
        })
        .unwrap_or_else(|| (full_path.clone(), media.mime_type.as_str()));

    let data = tokio::fs::read(&response_path)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let response = cot::http::Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type)
        .header(CONTENT_LENGTH, data.len().to_string())
        .header("Cache-Control", "public, max-age=86400")
        .body(Body::fixed(data))
        .expect("valid response");

    Ok(response)
}

// ---------------------------------------------------------------------------
// Player devices
// ---------------------------------------------------------------------------

async fn devices_heartbeat_handler(
    session: Session,
    db: Database,
    hub: Arc<PlayerDeviceHub>,
    Json(dto): Json<DeviceHeartbeatRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let Some(device_id) = normalize_device_id(&dto.device_id) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "invalid device id"));
    };

    let response = hub.heartbeat(
        user.id,
        &device_id,
        dto.user_agent.as_deref(),
        dto.playback_state,
    );
    Json(response).into_response()
}

async fn devices_poll_handler(
    session: Session,
    db: Database,
    hub: Arc<PlayerDeviceHub>,
    Json(dto): Json<DeviceHeartbeatRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let Some(device_id) = normalize_device_id(&dto.device_id) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "invalid device id"));
    };

    let response = hub.poll(
        user.id,
        &device_id,
        dto.user_agent.as_deref(),
        dto.playback_state,
    );
    Json(response).into_response()
}

async fn devices_select_handler(
    session: Session,
    db: Database,
    hub: Arc<PlayerDeviceHub>,
    Json(dto): Json<DeviceSelectRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let Some(target_device_id) = normalize_device_id(&dto.device_id) else {
        return Ok(json_error(StatusCode::BAD_REQUEST, "invalid device id"));
    };
    let current_device_id = dto
        .current_device_id
        .as_deref()
        .and_then(normalize_device_id)
        .unwrap_or_else(|| target_device_id.clone());

    let Some(response) = hub.select(user.id, &current_device_id, &target_device_id) else {
        return Ok(json_error(
            StatusCode::BAD_REQUEST,
            "target device is offline",
        ));
    };
    Json(response).into_response()
}

async fn devices_command_handler(
    session: Session,
    db: Database,
    hub: Arc<PlayerDeviceHub>,
    Json(dto): Json<DeviceCommandRequest>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let command = dto.command.trim();
    if command.is_empty() || command.len() > 64 {
        return Ok(json_error(StatusCode::BAD_REQUEST, "invalid command"));
    }
    let target_device_id = match dto.target_device_id.as_deref() {
        Some(raw) => {
            let Some(device_id) = normalize_device_id(raw) else {
                return Ok(json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid target device id",
                ));
            };
            Some(device_id)
        }
        None => None,
    };

    match hub.enqueue_command(user.id, target_device_id.as_deref(), command, dto.payload) {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(message) => Ok(json_error(StatusCode::BAD_REQUEST, message)),
    }
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

async fn history_list_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    query: cot::request::extractors::UrlQuery<HistoryQuery>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let page = query.0.page.unwrap_or(1).max(1);
    let per_page = query.0.limit.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) as i64 * per_page as i64;

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM furumusic__play_history WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    let rows = sqlx::query_as::<_, PlayHistoryRow>(
        r#"SELECT ph.id,
                  ph.track_id,
                  t.title::text AS track_title,
                  r.title::text AS release_title,
                  ph.played_at::text AS played_at,
                  ph.duration_listened,
                  ph.completed
           FROM furumusic__play_history ph
           JOIN furumusic__track t ON t.id = ph.track_id
           LEFT JOIN furumusic__release r ON r.id = t.release_id
           WHERE ph.user_id = $1
           ORDER BY ph.played_at DESC, ph.id DESC
           LIMIT $2 OFFSET $3"#,
    )
    .bind(user.id)
    .bind(per_page as i64)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    Json(PlayHistoryPage {
        items: rows
            .into_iter()
            .map(|row| PlayHistoryItem {
                id: row.id,
                track_id: row.track_id,
                track_title: row.track_title,
                release_title: row.release_title,
                played_at: row.played_at,
                duration_listened: row.duration_listened,
                completed: row.completed,
            })
            .collect(),
        total,
        page,
        per_page,
    })
    .into_response()
}

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

    if let Some(listened_seconds) = entry.duration_listened {
        let (config, _) = AppConfig::load_with_db(&db).await;
        match enqueue_lastfm_scrobble(
            pool,
            &config,
            user.id,
            entry.track_id,
            entry.started_at,
            listened_seconds,
        )
        .await
        {
            Ok(result) if result.queued => {
                tracing::info!(
                    user_id = user.id,
                    track_id = entry.track_id,
                    sent = result.sent,
                    "Queued Last.fm scrobble from play history"
                );
            }
            Ok(result) => {
                tracing::debug!(
                    user_id = user.id,
                    track_id = entry.track_id,
                    message = ?result.message,
                    "Play history did not queue Last.fm scrobble"
                );
            }
            Err(err) => {
                tracing::warn!(
                    user_id = user.id,
                    track_id = entry.track_id,
                    error = %err,
                    "Failed to queue Last.fm scrobble from play history"
                );
            }
        }
    }

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
                      rel.cover_file_id AS release_cover_file_id,
                      rel.id AS release_id,
                      rel.title::text AS release_title,
                      rel.year AS release_year,
                      COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                      mf.audio_format,
                      mf.audio_bitrate,
                      mf.audio_sample_rate,
                      mf.audio_bit_depth,
                      mf.file_size_bytes,
                  t.lastfm_listeners,
                  t.lastfm_playcount,
                  t.lastfm_rating,
                  t.lastfm_updated_at
               FROM furumusic__track t
               JOIN furumusic__release rel ON rel.id = t.release_id
               LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
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
            r#"SELECT id, title, track_number, disc_number, duration_seconds, cover_file_id,
                      release_cover_file_id, release_id, release_title, release_year, uploader_name, audio_format, audio_bitrate,
                      audio_sample_rate, audio_bit_depth, file_size_bytes, lastfm_listeners, lastfm_playcount, lastfm_rating, lastfm_updated_at FROM (
                SELECT t.id, t.title::text AS title, t.track_number, t.disc_number,
                       t.duration_seconds, t.cover_file_id,
                       rel.cover_file_id AS release_cover_file_id,
                       rel.id AS release_id,
                       rel.title::text AS release_title,
                       rel.year AS release_year,
                       COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                       mf.audio_format,
                       mf.audio_bitrate,
                       mf.audio_sample_rate,
                       mf.audio_bit_depth,
                       mf.file_size_bytes,
                       t.lastfm_listeners,
                       t.lastfm_playcount,
                       t.lastfm_rating,
                       t.lastfm_updated_at,
                       MAX(sim) AS similarity
                FROM (
                    SELECT id, title, title_sort, track_number, disc_number, duration_seconds, cover_file_id, release_id, audio_file_id,
                           lastfm_listeners, lastfm_playcount, lastfm_rating, lastfm_updated_at,
                           similarity(title_sort, $1) AS sim
                    FROM furumusic__track WHERE is_hidden = false AND title_sort % $1
                    UNION ALL
                    SELECT id, title, title_sort, track_number, disc_number, duration_seconds, cover_file_id, release_id, audio_file_id,
                           lastfm_listeners, lastfm_playcount, lastfm_rating, lastfm_updated_at,
                           0.01::real AS sim
                    FROM furumusic__track WHERE is_hidden = false AND title_sort ILIKE '%' || $1 || '%'
                ) t
                JOIN furumusic__release rel ON rel.id = t.release_id
                LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
                GROUP BY t.id, t.title, t.track_number, t.disc_number, t.duration_seconds, t.cover_file_id, rel.cover_file_id, rel.id, rel.title, rel.year,
                         mf.uploader_name, mf.audio_format, mf.audio_bitrate, mf.audio_sample_rate, mf.audio_bit_depth, mf.file_size_bytes,
                         t.lastfm_listeners, t.lastfm_playcount, t.lastfm_rating, t.lastfm_updated_at
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
            image_url: cover_variant_url(r.image_file_id, "medium"),
            release_count: r.release_count,
            track_count: r.track_count,
        })
        .collect();

    let release_ids: Vec<i64> = release_rows.iter().map(|r| r.id).collect();
    let mut release_uploaders = load_release_uploaders(pool, &release_ids)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;

    let releases: Vec<ReleaseCard> = release_rows
        .into_iter()
        .map(|r| ReleaseCard {
            id: r.id,
            title: r.title,
            release_type: r.release_type,
            year: r.year,
            cover_url: cover_variant_url(r.cover_file_id, "medium"),
            track_count: r.track_count,
            uploaders: release_uploaders.remove(&r.id).unwrap_or_default(),
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
                release_id: t.release_id,
                release_title: t.release_title,
                release_year: t.release_year,
                cover_url: track_cover_variant_url(
                    t.cover_file_id,
                    t.release_cover_file_id,
                    "medium",
                ),
                stream_url: format!("/api/player/stream/{tid}"),
                uploader_name: t.uploader_name,
                audio_format: t.audio_format,
                audio_bitrate: t.audio_bitrate,
                audio_sample_rate: t.audio_sample_rate,
                audio_bit_depth: t.audio_bit_depth,
                file_size_bytes: t.file_size_bytes,
                lastfm_listeners: t.lastfm_listeners,
                lastfm_playcount: t.lastfm_playcount,
                lastfm_rating: t.lastfm_rating,
                lastfm_updated_at: t.lastfm_updated_at,
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
        owner_name: Some(user.name),
        is_public: false,
        is_saved: false,
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
// GET /api/player/follows  — get followed artists for current user
// ---------------------------------------------------------------------------

async fn followed_artists_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };

    let rows = sqlx::query_as::<_, ArtistRow>(
        r#"SELECT a.id, a.name::text as name, a.image_file_id,
                  COALESCE(s.release_count, 0)::bigint AS release_count,
                  COALESCE(s.track_count, 0)::bigint AS track_count
           FROM furumusic__user_followed_artist ufa
           JOIN furumusic__artist a ON a.id = ufa.artist_id
           LEFT JOIN (
               SELECT ra.artist_id,
                      COUNT(DISTINCT r.id) AS release_count,
                      COUNT(t.id) AS track_count
               FROM furumusic__release_artist ra
               JOIN furumusic__release r ON r.id = ra.release_id AND r.is_hidden = false
               LEFT JOIN furumusic__track t ON t.release_id = r.id AND t.is_hidden = false
               WHERE ra.position = 0
               GROUP BY ra.artist_id
           ) s ON s.artist_id = a.id
           WHERE ufa.user_id = $1 AND a.is_hidden = false
           ORDER BY ufa.created_at DESC, a.name_sort"#,
    )
    .bind(user.id)
    .fetch_all(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    let artist_ids = rows.iter().map(|row| row.id).collect();
    let artists = rows
        .into_iter()
        .map(|r| ArtistCard {
            id: r.id,
            name: r.name,
            image_url: cover_variant_url(r.image_file_id, "small"),
            release_count: r.release_count,
            track_count: r.track_count,
        })
        .collect();

    Json(FollowedArtists {
        artist_ids,
        artists,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// POST /api/player/follows/toggle/{id}  — follow/unfollow artist
// ---------------------------------------------------------------------------

async fn toggle_follow_artist_handler(
    session: Session,
    db: Database,
    pool: &sqlx::PgPool,
    path: Path<PathId>,
) -> cot::Result<cot::response::Response> {
    let Some(user) = auth::get_session_user(&session, &db).await else {
        return Ok(json_error(StatusCode::UNAUTHORIZED, "not authenticated"));
    };
    let artist_id = path.0.id;

    let artist_exists: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM furumusic__artist WHERE id = $1 AND is_hidden = false")
            .bind(artist_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| cot::Error::internal(e.to_string()))?;

    if artist_exists.is_none() {
        return Ok(json_error(StatusCode::NOT_FOUND, "artist not found"));
    }

    let existing: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM furumusic__user_followed_artist WHERE user_id = $1 AND artist_id = $2",
    )
    .bind(user.id)
    .bind(artist_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| cot::Error::internal(e.to_string()))?;

    if existing.is_some() {
        sqlx::query(
            "DELETE FROM furumusic__user_followed_artist WHERE user_id = $1 AND artist_id = $2",
        )
        .bind(user.id)
        .bind(artist_id)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
        Json(FollowStatus { followed: false }).into_response()
    } else {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        sqlx::query(
            r#"INSERT INTO furumusic__user_followed_artist (user_id, artist_id, created_at)
               VALUES ($1, $2, $3)
               ON CONFLICT (user_id, artist_id) DO NOTHING"#,
        )
        .bind(user.id)
        .bind(artist_id)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(|e| cot::Error::internal(e.to_string()))?;
        Json(FollowStatus { followed: true }).into_response()
    }
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
                  r.cover_file_id as release_cover_file_id,
                  r.id as release_id,
                  r.title::text as release_title,
                  r.year as release_year,
                  COALESCE(mf.uploader_name, 'UFO')::text AS uploader_name,
                  mf.audio_format,
                  mf.audio_bitrate,
                  mf.audio_sample_rate,
                  mf.audio_bit_depth,
                  mf.file_size_bytes,
                  t.lastfm_listeners,
                  t.lastfm_playcount,
                  t.lastfm_rating,
                  t.lastfm_updated_at
           FROM furumusic__track t
           JOIN furumusic__release r ON r.id = t.release_id
           LEFT JOIN furumusic__media_file mf ON mf.id = t.audio_file_id
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
                release_id: t.release_id,
                release_title: t.release_title,
                release_year: t.release_year,
                cover_url: track_cover_variant_url(
                    t.cover_file_id,
                    t.release_cover_file_id,
                    "medium",
                ),
                stream_url: format!("/api/player/stream/{tid}"),
                uploader_name: t.uploader_name,
                audio_format: t.audio_format,
                audio_bitrate: t.audio_bitrate,
                audio_sample_rate: t.audio_sample_rate,
                audio_bit_depth: t.audio_bit_depth,
                file_size_bytes: t.file_size_bytes,
                lastfm_listeners: t.lastfm_listeners,
                lastfm_playcount: t.lastfm_playcount,
                lastfm_rating: t.lastfm_rating,
                lastfm_updated_at: t.lastfm_updated_at,
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
    device_hub: Arc<PlayerDeviceHub>,
}

impl PlayerApp {
    pub fn new(
        config: Arc<AppConfig>,
        scheduler_handle: Arc<tokio::sync::OnceCell<Arc<SchedulerHandle>>>,
    ) -> Self {
        Self {
            config,
            scheduler_handle,
            device_hub: Arc::new(PlayerDeviceHub::default()),
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
        let device_hub = Arc::clone(&self.device_hub);

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
            Route::with_handler_and_name(
                "/lastfm/status",
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
                            lastfm_status_handler(session, db, pg_pool).await
                        }
                    }
                }),
                "player_lastfm_status",
            ),
            Route::with_handler_and_name(
                "/lastfm/connect",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, request: cot::request::Request| {
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
                            lastfm_connect_handler(session, db, pg_pool, request).await
                        }
                    }
                }),
                "player_lastfm_connect",
            ),
            Route::with_handler_and_name(
                "/lastfm/callback",
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session,
                          db: Database,
                          query: cot::request::extractors::UrlQuery<LastfmCallbackQuery>| {
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
                            lastfm_callback_handler(session, db, pg_pool, query).await
                        }
                    }
                }),
                "player_lastfm_callback",
            ),
            Route::with_handler_and_name(
                "/lastfm/disconnect",
                post({
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
                            lastfm_disconnect_handler(session, db, pg_pool).await
                        }
                    }
                }),
                "player_lastfm_disconnect",
            ),
            Route::with_handler_and_name(
                "/lastfm/now-playing",
                post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<LastfmNowPlayingRequest>| {
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
                            lastfm_now_playing_handler(session, db, pg_pool, json).await
                        }
                    }
                }),
                "player_lastfm_now_playing",
            ),
            Route::with_handler_and_name(
                "/lastfm/scrobble",
                post({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session, db: Database, json: Json<LastfmScrobbleRequest>| {
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
                            lastfm_scrobble_handler(session, db, pg_pool, json).await
                        }
                    }
                }),
                "player_lastfm_scrobble",
            ),
            Route::with_handler_and_name(
                "/agent-queue",
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
                            agent_queue_handler(session, db, pg_pool).await
                        }
                    })
                },
                "player_agent_queue",
            ),
            // -- Torrent import widget --
            Route::with_handler_and_name(
                "/torrents",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    get(move |session: Session, db: Database| {
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        let torrent_service = Arc::clone(&torrent_service);
                        let scheduler_handle = Arc::clone(&scheduler_handle);
                        async move {
                            let Some(user) = auth::get_session_user(&session, &db).await else {
                                return Ok(json_error(
                                    StatusCode::UNAUTHORIZED,
                                    "not authenticated",
                                ));
                            };
                            let pg_pool = pool
                                .get_or_init(|| async {
                                    sqlx::postgres::PgPoolOptions::new()
                                        .max_connections(5)
                                        .connect(&pool_config.database_url)
                                        .await
                                        .expect("player pool")
                                })
                                .await;
                            let service = torrent_service
                                .get_or_init(|| async {
                                    Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                })
                                .await;
                            match service.list(pg_pool, user.id).await {
                                Ok(items) => Json(items).into_response(),
                                Err(err) => {
                                    Ok(json_error(StatusCode::BAD_REQUEST, &err.to_string()))
                                }
                            }
                        }
                    })
                },
                "player_torrent_list",
            ),
            Route::with_handler_and_name(
                "/torrents/session/{id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    get({
                        let pool = Arc::clone(&pool);
                        let pool_config = Arc::clone(&pool_config);
                        let torrent_service = Arc::clone(&torrent_service);
                        let scheduler_handle = Arc::clone(&scheduler_handle);
                        move |session: Session, db: Database, path: Path<PathStringId>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(user) = auth::get_session_user(&session, &db).await else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.details(pg_pool, user.id, &path.0.id).await {
                                    Ok(details) => Json(details).into_response(),
                                    Err(err) => {
                                        Ok(json_error(StatusCode::NOT_FOUND, &err.to_string()))
                                    }
                                }
                            }
                        }
                    })
                    .delete(
                        move |session: Session, db: Database, path: Path<PathStringId>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(user) = auth::get_session_user(&session, &db).await else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.remove(pg_pool, user.id, &path.0.id).await {
                                    Ok(()) => {
                                        Json(serde_json::json!({ "ok": true })).into_response()
                                    }
                                    Err(err) => {
                                        Ok(json_error(StatusCode::NOT_FOUND, &err.to_string()))
                                    }
                                }
                            }
                        },
                    )
                },
                "player_torrent_detail",
            ),
            Route::with_handler_and_name(
                "/torrents/preview",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    post(
                        move |session: Session, db: Database, json: Json<TorrentPreviewRequest>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(user) = auth::get_session_user(&session, &db).await else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.preview(pg_pool, user.id, json.0).await {
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
                "/uploads/local",
                {
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    post(
                        move |session: Session, db: Database, request: cot::request::Request| {
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let (live_config, _) = AppConfig::load_with_db(&db).await;
                                local_upload_handler(
                                    session,
                                    db,
                                    live_config,
                                    scheduler_handle,
                                    request,
                                )
                                .await
                            }
                        },
                    )
                },
                "player_local_upload",
            ),
            Route::with_handler_and_name(
                "/torrents/{id}/start",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    post(
                        move |session: Session,
                              db: Database,
                              path: Path<PathStringId>,
                              json: Json<TorrentStartRequest>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(user) = auth::get_session_user(&session, &db).await else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                let (live_config, _) = AppConfig::load_with_db(&db).await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service
                                    .start(
                                        pg_pool,
                                        &path.0.id,
                                        json.0.selected_files,
                                        live_config.agent_inbox_dir,
                                        user.id,
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
                "/torrents/{id}/pause",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    post(
                        move |session: Session, db: Database, path: Path<PathStringId>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(user) = auth::get_session_user(&session, &db).await else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.pause(pg_pool, user.id, &path.0.id).await {
                                    Ok(job) => Json(job).into_response(),
                                    Err(err) => {
                                        Ok(json_error(StatusCode::BAD_REQUEST, &err.to_string()))
                                    }
                                }
                            }
                        },
                    )
                },
                "player_torrent_pause",
            ),
            Route::with_handler_and_name(
                "/torrents/{id}/status",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    let torrent_service = Arc::clone(&torrent_service);
                    let scheduler_handle = Arc::clone(&self.scheduler_handle);
                    get(
                        move |session: Session, db: Database, path: Path<PathStringId>| {
                            let pool = Arc::clone(&pool);
                            let pool_config = Arc::clone(&pool_config);
                            let torrent_service = Arc::clone(&torrent_service);
                            let scheduler_handle = Arc::clone(&scheduler_handle);
                            async move {
                                let Some(user) = auth::get_session_user(&session, &db).await else {
                                    return Ok(json_error(
                                        StatusCode::UNAUTHORIZED,
                                        "not authenticated",
                                    ));
                                };
                                let pg_pool = pool
                                    .get_or_init(|| async {
                                        sqlx::postgres::PgPoolOptions::new()
                                            .max_connections(5)
                                            .connect(&pool_config.database_url)
                                            .await
                                            .expect("player pool")
                                    })
                                    .await;
                                let service = torrent_service
                                    .get_or_init(|| async {
                                        Arc::new(TorrentService::new(Arc::clone(&scheduler_handle)))
                                    })
                                    .await;
                                match service.status(pg_pool, user.id, &path.0.id).await {
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
            // -- Followed artists --
            Route::with_handler_and_name(
                "/follows",
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
                            followed_artists_handler(session, db, pg_pool).await
                        }
                    }
                }),
                "player_follows",
            ),
            // -- Follow/unfollow artist --
            Route::with_handler_and_name(
                "/follows/toggle/{id}",
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
                            toggle_follow_artist_handler(session, db, pg_pool, path).await
                        }
                    }
                }),
                "player_follow_toggle",
            ),
            // -- Audio stream --
            Route::with_handler_and_name(
                "/stream/{track_id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(
                        move |session: Session,
                              db: Database,
                              path: Path<PathTrackId>,
                              request: cot::request::Request| {
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
                                let (live_config, _) = AppConfig::load_with_db(&db).await;
                                stream_handler(session, db, pg_pool, &live_config, &request, path)
                                    .await
                            }
                        },
                    )
                },
                "player_stream",
            ),
            // -- Cover art --
            Route::with_handler_and_name(
                "/cover/{media_file_id}/{variant}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(
                        move |session: Session, db: Database, path: Path<PathMediaFileVariant>| {
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
                                let (live_config, _) = AppConfig::load_with_db(&db).await;
                                cover_variant_handler(session, db, pg_pool, &live_config, path)
                                    .await
                            }
                        },
                    )
                },
                "player_cover_variant",
            ),
            Route::with_handler_and_name(
                "/cover/{media_file_id}",
                {
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    get(
                        move |session: Session, db: Database, path: Path<PathMediaFileId>| {
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
                                let (live_config, _) = AppConfig::load_with_db(&db).await;
                                cover_handler(session, db, pg_pool, &live_config, path).await
                            }
                        },
                    )
                },
                "player_cover",
            ),
            // -- Active browser devices --
            Route::with_handler_and_name(
                "/devices/heartbeat",
                post({
                    let device_hub = Arc::clone(&device_hub);
                    move |session: Session, db: Database, json: Json<DeviceHeartbeatRequest>| {
                        let device_hub = Arc::clone(&device_hub);
                        async move { devices_heartbeat_handler(session, db, device_hub, json).await }
                    }
                }),
                "player_devices_heartbeat",
            ),
            Route::with_handler_and_name(
                "/devices/poll",
                post({
                    let device_hub = Arc::clone(&device_hub);
                    move |session: Session, db: Database, json: Json<DeviceHeartbeatRequest>| {
                        let device_hub = Arc::clone(&device_hub);
                        async move { devices_poll_handler(session, db, device_hub, json).await }
                    }
                }),
                "player_devices_poll",
            ),
            Route::with_handler_and_name(
                "/devices/active",
                post({
                    let device_hub = Arc::clone(&device_hub);
                    move |session: Session, db: Database, json: Json<DeviceSelectRequest>| {
                        let device_hub = Arc::clone(&device_hub);
                        async move { devices_select_handler(session, db, device_hub, json).await }
                    }
                }),
                "player_devices_active",
            ),
            Route::with_handler_and_name(
                "/devices/command",
                post({
                    let device_hub = Arc::clone(&device_hub);
                    move |session: Session, db: Database, json: Json<DeviceCommandRequest>| {
                        let device_hub = Arc::clone(&device_hub);
                        async move { devices_command_handler(session, db, device_hub, json).await }
                    }
                }),
                "player_devices_command",
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
                get({
                    let pool = Arc::clone(&pool);
                    let pool_config = Arc::clone(&pool_config);
                    move |session: Session,
                          db: Database,
                          query: cot::request::extractors::UrlQuery<HistoryQuery>| {
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
                            history_list_handler(session, db, pg_pool, query).await
                        }
                    }
                })
                .post({
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
