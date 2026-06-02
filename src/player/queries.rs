use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct HistoryEntry {
    pub(super) track_id: i64,
    pub(super) started_at: Option<i64>,
    pub(super) duration_listened: Option<i32>,
    pub(super) completed: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct HistoryQuery {
    pub(super) page: Option<i32>,
    pub(super) limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TracksByIdsRequest {
    pub(super) ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UserUploadsQuery {
    pub(super) limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreatePlaylistRequest {
    pub(super) title: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdatePlaylistRequest {
    pub(super) title: Option<String>,
    pub(super) description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AddTracksRequest {
    pub(super) track_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RemoveTrackRequest {
    pub(super) track_id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct PaginationQuery {
    pub(super) page: Option<i32>,
    pub(super) limit: Option<i32>,
    pub(super) mine: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PathId {
    pub(super) id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct PathStringId {
    pub(super) id: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PathRadioSeed {
    pub(super) kind: String,
    pub(super) id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct SearchQuery {
    pub(super) q: String,
    pub(super) limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct JamUserSearchQuery {
    pub(super) q: Option<String>,
    pub(super) limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PathTrackId {
    pub(super) track_id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct PathMediaFileId {
    pub(super) media_file_id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct PathMediaFileVariant {
    pub(super) media_file_id: i64,
    pub(super) variant: String,
}
