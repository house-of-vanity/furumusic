use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ArtistCard {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) image_url: Option<String>,
    pub(super) release_count: i64,
    pub(super) track_count: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct Paginated<T: Serialize> {
    pub(super) items: Vec<T>,
    pub(super) total: i64,
    pub(super) page: i32,
    pub(super) per_page: i32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ReleaseCard {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_type: String,
    pub(super) year: Option<i32>,
    pub(super) cover_url: Option<String>,
    pub(super) track_count: i64,
    pub(super) uploaders: Vec<UploaderSummary>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ArtistDetail {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) image_url: Option<String>,
    pub(super) total_track_count: i64,
    pub(super) total_play_count: i64,
    pub(super) releases: Vec<ReleaseCard>,
    pub(super) featured_tracks: Vec<ArtistAppearanceTrack>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ArtistRef {
    pub(super) id: i64,
    pub(super) name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct TrackItem {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) track_number: Option<i32>,
    pub(super) disc_number: Option<i32>,
    pub(super) duration_seconds: f64,
    pub(super) artists: Vec<ArtistRef>,
    pub(super) featured_artists: Vec<ArtistRef>,
    pub(super) release_id: i64,
    pub(super) release_title: String,
    pub(super) release_year: Option<i32>,
    pub(super) cover_url: Option<String>,
    pub(super) stream_url: String,
    pub(super) uploader_name: String,
    pub(super) audio_format: Option<String>,
    pub(super) audio_bitrate: Option<i32>,
    pub(super) audio_sample_rate: Option<i32>,
    pub(super) audio_bit_depth: Option<i32>,
    pub(super) file_size_bytes: Option<i64>,
    pub(super) lastfm_listeners: Option<i64>,
    pub(super) lastfm_playcount: Option<i64>,
    pub(super) lastfm_rating: Option<f64>,
    pub(super) lastfm_updated_at: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ArtistAppearanceTrack {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_id: i64,
    pub(super) release_title: String,
    pub(super) release_year: Option<i32>,
    pub(super) duration_seconds: f64,
    pub(super) artists: Vec<ArtistRef>,
    pub(super) featured_artists: Vec<ArtistRef>,
    pub(super) cover_url: Option<String>,
    pub(super) stream_url: String,
    pub(super) uploader_name: String,
    pub(super) audio_format: Option<String>,
    pub(super) audio_bitrate: Option<i32>,
    pub(super) audio_sample_rate: Option<i32>,
    pub(super) audio_bit_depth: Option<i32>,
    pub(super) file_size_bytes: Option<i64>,
    pub(super) lastfm_listeners: Option<i64>,
    pub(super) lastfm_playcount: Option<i64>,
    pub(super) lastfm_rating: Option<f64>,
    pub(super) lastfm_updated_at: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ReleaseDetail {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_type: String,
    pub(super) year: Option<i32>,
    pub(super) cover_url: Option<String>,
    pub(super) artists: Vec<ArtistRef>,
    pub(super) tracks: Vec<TrackItem>,
    pub(super) uploaders: Vec<UploaderSummary>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(super) struct UploaderSummary {
    pub(super) name: String,
    pub(super) track_count: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlaylistCard {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) track_count: i64,
    pub(super) is_own: bool,
    pub(super) owner_name: Option<String>,
    pub(super) is_public: bool,
    pub(super) is_saved: bool,
    pub(super) kind: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub(super) struct PlaybackStateDto {
    pub(super) current_track_id: Option<i64>,
    pub(super) position_ms: i32,
    pub(super) queue: Vec<i64>,
    pub(super) queue_position: i32,
    pub(super) shuffle: bool,
    pub(super) repeat_mode: String,
    pub(super) volume: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct DeviceHeartbeatRequest {
    pub(super) device_id: String,
    pub(super) user_agent: Option<String>,
    pub(super) playback_state: Option<PlayerDevicePlaybackStateDto>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct DeviceSelectRequest {
    pub(super) device_id: String,
    pub(super) current_device_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct DeviceCommandRequest {
    pub(super) target_device_id: Option<String>,
    pub(super) command: String,
    #[serde(default)]
    pub(super) payload: serde_json::Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlayerDeviceDto {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) is_current: bool,
    pub(super) is_active: bool,
    pub(super) last_seen_ms: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlayerDeviceCommandDto {
    pub(super) id: String,
    pub(super) command: String,
    pub(super) payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(super) struct PlayerDevicePlaybackStateDto {
    pub(super) track: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) tracks: Vec<serde_json::Value>,
    pub(super) index: i32,
    pub(super) position_seconds: f64,
    pub(super) duration_seconds: f64,
    pub(super) paused: bool,
    pub(super) shuffle: bool,
    pub(super) repeat_mode: String,
    pub(super) volume: f64,
    #[serde(default)]
    pub(super) updated_at_ms: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlayerDevicesResponse {
    pub(super) device_id: String,
    pub(super) active_device_id: Option<String>,
    pub(super) devices: Vec<PlayerDeviceDto>,
    pub(super) playback_state: Option<PlayerDevicePlaybackStateDto>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlayerDevicePollResponse {
    pub(super) device_id: String,
    pub(super) active_device_id: Option<String>,
    pub(super) devices: Vec<PlayerDeviceDto>,
    pub(super) commands: Vec<PlayerDeviceCommandDto>,
    pub(super) playback_state: Option<PlayerDevicePlaybackStateDto>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlaylistDetail {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) description: Option<String>,
    pub(super) is_own: bool,
    pub(super) owner_name: Option<String>,
    pub(super) is_public: bool,
    pub(super) is_saved: bool,
    pub(super) kind: String,
    pub(super) tracks: Vec<TrackItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct SearchResults {
    pub(super) artists: Vec<ArtistCard>,
    pub(super) releases: Vec<ReleaseCard>,
    pub(super) tracks: Vec<TrackItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct UserStats {
    pub(super) liked_tracks: i64,
    pub(super) playlists: i64,
    pub(super) plays: i64,
    pub(super) listened_minutes: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct UserProfile {
    pub(super) name: String,
    pub(super) role: String,
    pub(super) stats: UserStats,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct LastfmStatus {
    pub(super) configured: bool,
    pub(super) connected: bool,
    pub(super) username: Option<String>,
    pub(super) reauth_required: bool,
    pub(super) last_error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct LastfmActionResponse {
    pub(super) ok: bool,
    pub(super) queued: bool,
    pub(super) sent: bool,
    pub(super) message: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LastfmNowPlayingRequest {
    pub(super) track_id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct LastfmScrobbleRequest {
    pub(super) track_id: i64,
    pub(super) started_at: Option<i64>,
    pub(super) listened_seconds: i32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AgentQueueStatus {
    pub(super) queued_count: i64,
    pub(super) processing_count: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlayHistoryItem {
    pub(super) id: i64,
    pub(super) track_id: i64,
    pub(super) track_title: String,
    pub(super) release_title: Option<String>,
    pub(super) played_at: String,
    pub(super) duration_listened: Option<i32>,
    pub(super) completed: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct PlayHistoryPage {
    pub(super) items: Vec<PlayHistoryItem>,
    pub(super) total: i64,
    pub(super) page: i32,
    pub(super) per_page: i32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct LikeStatus {
    pub(super) liked: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct LikedIds {
    pub(super) track_ids: Vec<i64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct FollowStatus {
    pub(super) followed: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct FollowedArtists {
    pub(super) artist_ids: Vec<i64>,
    pub(super) artists: Vec<ArtistCard>,
}
