#[derive(sqlx::FromRow)]
pub(super) struct ArtistRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) image_file_id: Option<i64>,
    pub(super) release_count: i64,
    pub(super) track_count: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct CountRow {
    pub(super) count: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct ReleaseRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_type: String,
    pub(super) year: Option<i32>,
    pub(super) cover_file_id: Option<i64>,
    pub(super) track_count: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct ArtistBriefRow {
    pub(super) id: i64,
    pub(super) name: String,
}

#[derive(sqlx::FromRow)]
pub(super) struct TrackRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) track_number: Option<i32>,
    pub(super) disc_number: Option<i32>,
    pub(super) duration_seconds: f64,
    pub(super) cover_file_id: Option<i64>,
    pub(super) release_cover_file_id: Option<i64>,
    pub(super) uploader_name: String,
    pub(super) audio_format: Option<String>,
    pub(super) audio_bitrate: Option<i32>,
    pub(super) audio_sample_rate: Option<i32>,
    pub(super) audio_bit_depth: Option<i32>,
    pub(super) file_size_bytes: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub(super) struct TrackArtistRow {
    pub(super) track_id: i64,
    pub(super) artist_id: i64,
    pub(super) artist_name: String,
    pub(super) role: String,
}

#[derive(sqlx::FromRow)]
pub(super) struct MediaFileRow {
    pub(super) file_path: String,
    pub(super) mime_type: String,
    pub(super) file_size_bytes: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlaybackStateRow {
    pub(super) current_track_id: Option<i64>,
    pub(super) position_ms: i32,
    pub(super) queue_json: String,
    pub(super) queue_position: i32,
    pub(super) shuffle: bool,
    pub(super) repeat_mode: String,
    pub(super) volume: f64,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlaylistRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) track_count: i64,
    pub(super) is_own: bool,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlaylistInfoRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) description: Option<String>,
    pub(super) owner_id: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlaylistTrackRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) track_number: Option<i32>,
    pub(super) disc_number: Option<i32>,
    pub(super) duration_seconds: f64,
    pub(super) cover_file_id: Option<i64>,
    pub(super) release_cover_file_id: Option<i64>,
    pub(super) uploader_name: String,
    pub(super) audio_format: Option<String>,
    pub(super) audio_bitrate: Option<i32>,
    pub(super) audio_sample_rate: Option<i32>,
    pub(super) audio_bit_depth: Option<i32>,
    pub(super) file_size_bytes: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub(super) struct AppearanceTrackRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_id: i64,
    pub(super) release_title: String,
    pub(super) duration_seconds: f64,
    pub(super) cover_file_id: Option<i64>,
    pub(super) release_cover_file_id: Option<i64>,
    pub(super) uploader_name: String,
    pub(super) audio_format: Option<String>,
    pub(super) audio_bitrate: Option<i32>,
    pub(super) audio_sample_rate: Option<i32>,
    pub(super) audio_bit_depth: Option<i32>,
    pub(super) file_size_bytes: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub(super) struct SearchArtistRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) image_file_id: Option<i64>,
    pub(super) release_count: i64,
    pub(super) track_count: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct SearchReleaseRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_type: String,
    pub(super) year: Option<i32>,
    pub(super) cover_file_id: Option<i64>,
    pub(super) track_count: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct SearchTrackRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) track_number: Option<i32>,
    pub(super) disc_number: Option<i32>,
    pub(super) duration_seconds: f64,
    pub(super) cover_file_id: Option<i64>,
    pub(super) release_cover_file_id: Option<i64>,
    pub(super) uploader_name: String,
    pub(super) audio_format: Option<String>,
    pub(super) audio_bitrate: Option<i32>,
    pub(super) audio_sample_rate: Option<i32>,
    pub(super) audio_bit_depth: Option<i32>,
    pub(super) file_size_bytes: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub(super) struct ReleaseUploaderRow {
    pub(super) release_id: i64,
    pub(super) uploader_name: String,
    pub(super) track_count: i64,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlayHistoryRow {
    pub(super) id: i64,
    pub(super) track_id: i64,
    pub(super) track_title: String,
    pub(super) release_title: Option<String>,
    pub(super) played_at: String,
    pub(super) duration_listened: Option<i32>,
    pub(super) completed: bool,
}

#[derive(sqlx::FromRow)]
pub(super) struct ReleaseInfoRow {
    pub(super) id: i64,
    pub(super) title: String,
    pub(super) release_type: String,
    pub(super) year: Option<i32>,
    pub(super) cover_file_id: Option<i64>,
}
