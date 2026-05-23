/// Music library models and migrations.
///
/// This module contains all database models related to the music library:
/// content (files, artists, releases, tracks, genres), user interactions
/// (likes, follows, playlists, play history, playback state), and the
/// AI-agent processing queue.

use cot::db::{Auto, Database, LimitedString, Model};

// ---------------------------------------------------------------------------
// MediaFile — audio files and cover art on disk
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct MediaFile {
    #[model(primary_key)]
    pub id: Auto<i64>,
    /// "audio" or "cover_art"
    pub file_type: LimitedString<32>,
    /// Relative path on disk from the media root
    pub file_path: String,
    /// Original filename as uploaded
    pub original_filename: LimitedString<255>,
    /// MIME type, e.g. "audio/flac", "image/jpeg"
    pub mime_type: LimitedString<100>,
    /// File size in bytes
    pub file_size_bytes: i64,
    /// SHA-256 hex digest for dedup
    pub sha256_hash: LimitedString<64>,
    // Audio-specific fields (NULL for non-audio files)
    /// e.g. "mp3", "flac", "ogg", "wav"
    pub audio_format: Option<String>,
    /// Bitrate in kbps
    pub audio_bitrate: Option<i32>,
    /// Sample rate in Hz
    pub audio_sample_rate: Option<i32>,
    /// Bit depth (16, 24, 32)
    pub audio_bit_depth: Option<i32>,
    pub created_at: LimitedString<32>,
}

// ---------------------------------------------------------------------------
// Artist
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct Artist {
    #[model(primary_key)]
    pub id: Auto<i64>,
    /// Canonical display name
    pub name: LimitedString<255>,
    /// Normalized for search/dedup (lowercase, stripped)
    pub name_sort: LimitedString<255>,
    /// FK → media_file (artist image), nullable
    pub image_file_id: Option<i64>,
    pub is_hidden: bool,
    /// NULL = human-created, non-NULL = LLM model that created it
    pub model_name: Option<String>,
    pub created_at: LimitedString<32>,
    pub updated_at: LimitedString<32>,
}

fn now_iso() -> LimitedString<32> {
    LimitedString::new(&chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap()
}

fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

impl Artist {
    pub async fn list_all(db: &Database) -> cot::db::Result<Vec<Self>> {
        Self::objects().all(db).await
    }

    pub async fn get_by_id(db: &Database, artist_id: i64) -> cot::db::Result<Option<Self>> {
        Self::get_by_primary_key(db, Auto::Fixed(artist_id)).await
    }

    pub async fn create(
        db: &Database,
        name: &str,
        model_name: Option<&str>,
    ) -> cot::db::Result<Self> {
        let now = now_iso();
        let mut artist = Self {
            id: Auto::auto(),
            name: LimitedString::new(name).unwrap(),
            name_sort: LimitedString::new(&normalize_name(name)).unwrap(),
            image_file_id: None,
            is_hidden: false,
            model_name: model_name.map(str::to_owned),
            created_at: now.clone(),
            updated_at: now,
        };
        artist.insert(db).await?;
        Ok(artist)
    }

    pub async fn update_name(
        &mut self,
        db: &Database,
        name: &str,
    ) -> cot::db::Result<()> {
        self.name = LimitedString::new(name).unwrap();
        self.name_sort = LimitedString::new(&normalize_name(name)).unwrap();
        self.updated_at = now_iso();
        self.save(db).await
    }

    pub async fn set_image_file_id(
        &mut self,
        db: &Database,
        file_id: Option<i64>,
    ) -> cot::db::Result<()> {
        self.image_file_id = file_id;
        self.updated_at = now_iso();
        self.save(db).await
    }

    pub async fn delete_by_id(db: &Database, artist_id: i64) -> cot::db::Result<()> {
        cot::db::query!(Artist, $id == Auto::Fixed(artist_id))
            .delete(db)
            .await?;
        Ok(())
    }

    pub fn id_val(&self) -> i64 {
        self.id.unwrap()
    }

    pub fn name_str(&self) -> &str {
        &self.name
    }

    pub fn is_hidden(&self) -> bool {
        self.is_hidden
    }
}

// ---------------------------------------------------------------------------
// Release (album / single / EP / etc.)
// ---------------------------------------------------------------------------

pub const RELEASE_TYPES: &[(&str, &str, &str)] = &[
    ("album", "Album", "Альбом"),
    ("single", "Single", "Сингл"),
    ("ep", "EP", "EP"),
    ("compilation", "Compilation", "Сборник"),
    ("mixtape", "Mixtape", "Микстейп"),
    ("live", "Live", "Концерт"),
    ("soundtrack", "Soundtrack", "Саундтрек"),
    ("remix", "Remix", "Ремикс"),
    ("demo", "Demo", "Демо"),
];

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct Release {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub title: LimitedString<255>,
    /// Normalized for search/dedup
    pub title_sort: LimitedString<255>,
    /// One of: album, single, ep, compilation, mixtape, live, soundtrack, remix, demo
    pub release_type: LimitedString<32>,
    pub year: Option<i32>,
    /// FK → media_file (cover art), nullable
    pub cover_file_id: Option<i64>,
    pub total_tracks: Option<i32>,
    pub total_discs: Option<i32>,
    pub is_hidden: bool,
    /// NULL = human-created, non-NULL = LLM model that created it
    pub model_name: Option<String>,
    pub created_at: LimitedString<32>,
    pub updated_at: LimitedString<32>,
}

#[allow(dead_code)]
impl Release {
    pub async fn list_all(db: &Database) -> cot::db::Result<Vec<Self>> {
        Self::objects().all(db).await
    }

    pub async fn get_by_id(db: &Database, release_id: i64) -> cot::db::Result<Option<Self>> {
        Self::get_by_primary_key(db, Auto::Fixed(release_id)).await
    }

    pub async fn create(
        db: &Database,
        title: &str,
        release_type: &str,
        year: Option<i32>,
        model_name: Option<&str>,
    ) -> cot::db::Result<Self> {
        let now = now_iso();
        let mut release = Self {
            id: Auto::auto(),
            title: LimitedString::new(title).unwrap(),
            title_sort: LimitedString::new(&normalize_name(title)).unwrap(),
            release_type: LimitedString::new(release_type).unwrap(),
            year,
            cover_file_id: None,
            total_tracks: None,
            total_discs: None,
            is_hidden: false,
            model_name: model_name.map(str::to_owned),
            created_at: now.clone(),
            updated_at: now,
        };
        release.insert(db).await?;
        Ok(release)
    }

    pub async fn update_fields(
        &mut self,
        db: &Database,
        title: &str,
        release_type: &str,
        year: Option<i32>,
    ) -> cot::db::Result<()> {
        self.title = LimitedString::new(title).unwrap();
        self.title_sort = LimitedString::new(&normalize_name(title)).unwrap();
        self.release_type = LimitedString::new(release_type).unwrap();
        self.year = year;
        self.updated_at = now_iso();
        self.save(db).await
    }

    pub async fn delete_by_id(db: &Database, release_id: i64) -> cot::db::Result<()> {
        // Also clean up release_artist links
        cot::db::query!(ReleaseArtist, $release_id == release_id)
            .delete(db)
            .await?;
        cot::db::query!(Release, $id == Auto::Fixed(release_id))
            .delete(db)
            .await?;
        Ok(())
    }

    pub fn id_val(&self) -> i64 {
        self.id.unwrap()
    }

    pub fn title_str(&self) -> &str {
        &self.title
    }

    pub fn release_type_str(&self) -> &str {
        &self.release_type
    }

    pub fn year_val(&self) -> Option<i32> {
        self.year
    }

    pub fn year_display(&self) -> String {
        self.year.map(|y| y.to_string()).unwrap_or_default()
    }

    pub fn is_hidden(&self) -> bool {
        self.is_hidden
    }
}

// ---------------------------------------------------------------------------
// ReleaseArtist — M2M between releases and artists
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct ReleaseArtist {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub release_id: i64,
    pub artist_id: i64,
    /// Display order
    pub position: i32,
}

impl ReleaseArtist {
    pub async fn find_by_release(db: &Database, release_id: i64) -> cot::db::Result<Vec<Self>> {
        cot::db::query!(ReleaseArtist, $release_id == release_id)
            .all(db)
            .await
    }

    pub async fn find_by_artist(db: &Database, artist_id: i64) -> cot::db::Result<Vec<Self>> {
        cot::db::query!(ReleaseArtist, $artist_id == artist_id)
            .all(db)
            .await
    }

    pub async fn count_by_artist(db: &Database, artist_id: i64) -> cot::db::Result<u64> {
        cot::db::query!(ReleaseArtist, $artist_id == artist_id)
            .count(db)
            .await
    }

    pub async fn set_artists(
        db: &Database,
        release_id: i64,
        artist_ids: &[i64],
    ) -> cot::db::Result<()> {
        // Remove existing links
        cot::db::query!(ReleaseArtist, $release_id == release_id)
            .delete(db)
            .await?;
        // Insert new links
        for (pos, &aid) in artist_ids.iter().enumerate() {
            let mut link = Self {
                id: Auto::auto(),
                release_id,
                artist_id: aid,
                position: pos as i32,
            };
            link.insert(db).await?;
        }
        Ok(())
    }

    pub fn artist_id(&self) -> i64 {
        self.artist_id
    }

    pub fn release_id(&self) -> i64 {
        self.release_id
    }
}

// ---------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct Track {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub title: LimitedString<255>,
    /// Normalized for search/dedup
    pub title_sort: LimitedString<255>,
    /// FK → release
    pub release_id: i64,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    /// Duration in seconds (float stored as f64)
    pub duration_seconds: f64,
    /// FK → media_file (audio)
    pub audio_file_id: i64,
    /// FK → media_file (cover art), nullable — falls back to release cover
    pub cover_file_id: Option<i64>,
    pub year: Option<i32>,
    pub is_hidden: bool,
    /// NULL = human-created, non-NULL = LLM model that created it
    pub model_name: Option<String>,
    pub created_at: LimitedString<32>,
    pub updated_at: LimitedString<32>,
}

// ---------------------------------------------------------------------------
// TrackArtist — M2M between tracks and artists (with role)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct TrackArtist {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub track_id: i64,
    pub artist_id: i64,
    /// "main", "featuring", "remixer", "producer"
    pub role: LimitedString<32>,
    /// Display order
    pub position: i32,
}

impl TrackArtist {
    pub async fn count_by_artist(db: &Database, artist_id: i64) -> cot::db::Result<u64> {
        cot::db::query!(TrackArtist, $artist_id == artist_id)
            .count(db)
            .await
    }

    pub async fn create(
        db: &Database,
        track_id: i64,
        artist_id: i64,
        role: &str,
        position: i32,
    ) -> cot::db::Result<Self> {
        let mut link = Self {
            id: Auto::auto(),
            track_id,
            artist_id,
            role: LimitedString::new(role).unwrap(),
            position,
        };
        link.insert(db).await?;
        Ok(link)
    }
}

// ---------------------------------------------------------------------------
// Genre
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct Genre {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub name: LimitedString<100>,
    /// Normalized for dedup (lowercase, trimmed)
    pub name_normalized: LimitedString<100>,
}

// ---------------------------------------------------------------------------
// TrackGenre — M2M between tracks and genres
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct TrackGenre {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub track_id: i64,
    pub genre_id: i64,
}

// ---------------------------------------------------------------------------
// UserLikedTrack
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct UserLikedTrack {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub user_id: i64,
    pub track_id: i64,
    pub created_at: LimitedString<32>,
}

// ---------------------------------------------------------------------------
// UserFollowedArtist
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct UserFollowedArtist {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub user_id: i64,
    pub artist_id: i64,
    pub created_at: LimitedString<32>,
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct Playlist {
    #[model(primary_key)]
    pub id: Auto<i64>,
    /// FK → user (owner/creator)
    pub owner_id: i64,
    pub title: LimitedString<255>,
    pub description: Option<String>,
    pub is_public: bool,
    /// FK → media_file (custom cover), nullable
    pub cover_file_id: Option<i64>,
    /// FK → playlist (original, if this is a fork), nullable
    pub forked_from_id: Option<i64>,
    pub created_at: LimitedString<32>,
    pub updated_at: LimitedString<32>,
}

// ---------------------------------------------------------------------------
// PlaylistTrack
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct PlaylistTrack {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub playlist_id: i64,
    pub track_id: i64,
    /// Order within the playlist
    pub position: i32,
    pub added_at: LimitedString<32>,
    /// FK → user (who added this track)
    pub added_by_user_id: i64,
}

// ---------------------------------------------------------------------------
// SavedPlaylist — user "follows" someone else's playlist
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct SavedPlaylist {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub user_id: i64,
    pub playlist_id: i64,
    pub saved_at: LimitedString<32>,
}

// ---------------------------------------------------------------------------
// PlayHistory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct PlayHistory {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub user_id: i64,
    pub track_id: i64,
    pub played_at: LimitedString<32>,
    /// How many seconds the user actually listened
    pub duration_listened: Option<i32>,
    /// Did the user listen to the end?
    pub completed: bool,
}

// ---------------------------------------------------------------------------
// PlaybackState — one per user, current queue + position
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[cot::db::model]
pub struct PlaybackState {
    #[model(primary_key)]
    pub id: Auto<i64>,
    pub user_id: i64,
    /// FK → track (currently playing), nullable
    pub current_track_id: Option<i64>,
    /// Current position in the track, in milliseconds
    pub position_ms: i32,
    /// JSON array of track IDs
    pub queue_json: String,
    /// Index of the current track in the queue
    pub queue_position: i32,
    pub shuffle: bool,
    /// "off", "all", "one"
    pub repeat_mode: LimitedString<16>,
    /// Volume level 0.0 – 1.0
    pub volume: f64,
    pub updated_at: LimitedString<32>,
}

impl Track {
    pub async fn list_all(db: &Database) -> cot::db::Result<Vec<Self>> {
        Self::objects().all(db).await
    }

    pub async fn create(
        db: &Database,
        title: &str,
        release_id: i64,
        track_number: Option<i32>,
        disc_number: Option<i32>,
        duration_seconds: f64,
        audio_file_id: i64,
        year: Option<i32>,
        model_name: Option<&str>,
    ) -> cot::db::Result<Self> {
        let now = now_iso();
        let mut track = Self {
            id: Auto::auto(),
            title: LimitedString::new(title).unwrap(),
            title_sort: LimitedString::new(&normalize_name(title)).unwrap(),
            release_id,
            track_number,
            disc_number,
            duration_seconds,
            audio_file_id,
            cover_file_id: None,
            year,
            is_hidden: false,
            model_name: model_name.map(str::to_owned),
            created_at: now.clone(),
            updated_at: now,
        };
        track.insert(db).await?;
        Ok(track)
    }

    pub fn id_val(&self) -> i64 {
        self.id.unwrap()
    }
}

#[allow(dead_code)]
impl MediaFile {
    pub async fn create(
        db: &Database,
        file_type: &str,
        file_path: &str,
        original_filename: &str,
        mime_type: &str,
        file_size_bytes: i64,
        sha256_hash: &str,
        audio_format: Option<&str>,
        audio_bitrate: Option<i32>,
        audio_sample_rate: Option<i32>,
        audio_bit_depth: Option<i32>,
    ) -> cot::db::Result<Self> {
        let now = now_iso();
        let mut mf = Self {
            id: Auto::auto(),
            file_type: LimitedString::new(file_type).unwrap(),
            file_path: file_path.to_owned(),
            original_filename: LimitedString::new(original_filename).unwrap(),
            mime_type: LimitedString::new(mime_type).unwrap(),
            file_size_bytes,
            sha256_hash: LimitedString::new(sha256_hash).unwrap(),
            audio_format: audio_format.map(str::to_owned),
            audio_bitrate,
            audio_sample_rate,
            audio_bit_depth,
            created_at: now,
        };
        mf.insert(db).await?;
        Ok(mf)
    }

    pub fn id_val(&self) -> i64 {
        self.id.unwrap()
    }

    pub async fn list_all(db: &Database) -> cot::db::Result<Vec<Self>> {
        Self::objects().all(db).await
    }

    pub async fn get_by_id(db: &Database, id: i64) -> cot::db::Result<Option<Self>> {
        Self::get_by_primary_key(db, Auto::Fixed(id)).await
    }

    pub async fn delete_by_id(db: &Database, id: i64) -> cot::db::Result<()> {
        db.raw(&format!(
            "DELETE FROM furumusic__media_file WHERE id = {}",
            id
        ))
        .await?;
        Ok(())
    }

    pub fn file_type_str(&self) -> &str {
        &self.file_type
    }

    pub fn file_path_str(&self) -> &str {
        &self.file_path
    }

    pub fn original_filename_str(&self) -> &str {
        &self.original_filename
    }

    pub fn mime_type_str(&self) -> &str {
        &self.mime_type
    }

    pub fn sha256_hash_str(&self) -> &str {
        &self.sha256_hash
    }

    pub fn audio_format_str(&self) -> &str {
        self.audio_format.as_deref().unwrap_or("")
    }

    pub fn created_at_str(&self) -> &str {
        &self.created_at
    }

    pub fn file_size_display(&self) -> String {
        let bytes = self.file_size_bytes;
        if bytes >= 1_073_741_824 {
            format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
        } else if bytes >= 1_048_576 {
            format!("{:.1} MB", bytes as f64 / 1_048_576.0)
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{bytes} B")
        }
    }
}

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

pub mod db_migrations {
    use cot::db::migrations::{self, Field, Operation, SyncDynMigration};
    use cot::db::{DatabaseField, Identifier, LimitedString};

    // -- M0006: create furumusic__media_file ----------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0006CreateMediaFile;

    impl migrations::Migration for M0006CreateMediaFile {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0006_create_media_file";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0005_oidc_link_indexes",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__media_file"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("file_type"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("file_path"), <String as DatabaseField>::TYPE),
                    Field::new(Identifier::new("original_filename"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("mime_type"), <LimitedString<100> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("file_size_bytes"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("sha256_hash"), <LimitedString<64> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("audio_format"), <LimitedString<32> as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("audio_bitrate"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("audio_sample_rate"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("audio_bit_depth"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0007: create furumusic__artist --------------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0007CreateArtist;

    impl migrations::Migration for M0007CreateArtist {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0007_create_artist";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0006_create_media_file",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__artist"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("name"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("name_sort"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("image_file_id"), <i64 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("is_hidden"), <bool as DatabaseField>::TYPE),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("updated_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0008: create furumusic__release -------------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0008CreateRelease;

    impl migrations::Migration for M0008CreateRelease {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0008_create_release";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0007_create_artist",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__release"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("title"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("title_sort"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("release_type"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("year"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("cover_file_id"), <i64 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("total_tracks"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("total_discs"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("is_hidden"), <bool as DatabaseField>::TYPE),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("updated_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0009: create furumusic__release_artist ------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0009CreateReleaseArtist;

    impl migrations::Migration for M0009CreateReleaseArtist {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0009_create_release_artist";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0008_create_release",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__release_artist"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("release_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("artist_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("position"), <i32 as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0010: create furumusic__track ---------------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0010CreateTrack;

    impl migrations::Migration for M0010CreateTrack {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0010_create_track";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0009_create_release_artist",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__track"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("title"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("title_sort"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("release_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("track_number"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("disc_number"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("duration_seconds"), <f64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("audio_file_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("cover_file_id"), <i64 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("year"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("is_hidden"), <bool as DatabaseField>::TYPE),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("updated_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0011: create furumusic__track_artist --------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0011CreateTrackArtist;

    impl migrations::Migration for M0011CreateTrackArtist {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0011_create_track_artist";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0010_create_track",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__track_artist"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("track_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("artist_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("role"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("position"), <i32 as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0012: create furumusic__genre + furumusic__track_genre ---------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0012CreateGenreTables;

    impl migrations::Migration for M0012CreateGenreTables {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0012_create_genre_tables";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0011_create_track_artist",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__genre"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("name"), <LimitedString<100> as DatabaseField>::TYPE)
                        .unique(),
                    Field::new(Identifier::new("name_normalized"), <LimitedString<100> as DatabaseField>::TYPE),
                ])
                .build(),
            Operation::create_model()
                .table_name(Identifier::new("furumusic__track_genre"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("track_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("genre_id"), <i64 as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0013: create furumusic__user_liked_track ----------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0013CreateUserLikedTrack;

    impl migrations::Migration for M0013CreateUserLikedTrack {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0013_create_user_liked_track";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0012_create_genre_tables",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__user_liked_track"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("user_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("track_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0014: create furumusic__user_followed_artist ------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0014CreateUserFollowedArtist;

    impl migrations::Migration for M0014CreateUserFollowedArtist {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0014_create_user_followed_artist";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0013_create_user_liked_track",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__user_followed_artist"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("user_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("artist_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0015: create playlist tables ----------------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0015CreatePlaylistTables;

    impl migrations::Migration for M0015CreatePlaylistTables {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0015_create_playlist_tables";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0014_create_user_followed_artist",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__playlist"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("owner_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("title"), <LimitedString<255> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("description"), <String as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("is_public"), <bool as DatabaseField>::TYPE),
                    Field::new(Identifier::new("cover_file_id"), <i64 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("forked_from_id"), <i64 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("updated_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
            Operation::create_model()
                .table_name(Identifier::new("furumusic__playlist_track"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("playlist_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("track_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("position"), <i32 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("added_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("added_by_user_id"), <i64 as DatabaseField>::TYPE),
                ])
                .build(),
            Operation::create_model()
                .table_name(Identifier::new("furumusic__saved_playlist"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("user_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("playlist_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("saved_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0016: create furumusic__play_history --------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0016CreatePlayHistory;

    impl migrations::Migration for M0016CreatePlayHistory {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0016_create_play_history";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0015_create_playlist_tables",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__play_history"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("user_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("track_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("played_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("duration_listened"), <i32 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("completed"), <bool as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0017: create furumusic__playback_state ------------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0017CreatePlaybackState;

    impl migrations::Migration for M0017CreatePlaybackState {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0017_create_playback_state";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0016_create_play_history",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__playback_state"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("user_id"), <i64 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("current_track_id"), <i64 as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("position_ms"), <i32 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("queue_json"), <String as DatabaseField>::TYPE),
                    Field::new(Identifier::new("queue_position"), <i32 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("shuffle"), <bool as DatabaseField>::TYPE),
                    Field::new(Identifier::new("repeat_mode"), <LimitedString<16> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("updated_at"), <LimitedString<32> as DatabaseField>::TYPE),
                ])
                .build(),
        ];
    }

    // -- M0018: create furumusic__processing_task -----------------------------

    #[derive(Debug, Copy, Clone)]
    pub struct M0018CreateProcessingTask;

    impl migrations::Migration for M0018CreateProcessingTask {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0018_create_processing_task";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0017_create_playback_state",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::create_model()
                .table_name(Identifier::new("furumusic__processing_task"))
                .fields(&[
                    Field::new(Identifier::new("id"), <i64 as DatabaseField>::TYPE)
                        .primary_key()
                        .auto(),
                    Field::new(Identifier::new("status"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("task_type"), <LimitedString<64> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("input_path"), <String as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("context_json"), <String as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("result_json"), <String as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("error_message"), <String as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("attempts"), <i32 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("max_attempts"), <i32 as DatabaseField>::TYPE),
                    Field::new(Identifier::new("created_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("updated_at"), <LimitedString<32> as DatabaseField>::TYPE),
                    Field::new(Identifier::new("started_at"), <LimitedString<32> as DatabaseField>::TYPE)
                        .set_null(true),
                    Field::new(Identifier::new("completed_at"), <LimitedString<32> as DatabaseField>::TYPE)
                        .set_null(true),
                ])
                .build(),
        ];
    }

    // -- M0019: indexes for all music tables ----------------------------------

    #[cot::db::migrations::migration_op]
    async fn create_music_indexes(
        ctx: migrations::MigrationContext<'_>,
    ) -> cot::db::Result<()> {
        let stmts = [
            // media_file: lookup by hash for dedup
            "CREATE INDEX idx_media_file_sha256 ON furumusic__media_file (sha256_hash)",
            // media_file: filter by type
            "CREATE INDEX idx_media_file_type ON furumusic__media_file (file_type)",

            // artist: search by normalized name
            "CREATE INDEX idx_artist_name_sort ON furumusic__artist (name_sort)",

            // release: search by normalized title
            "CREATE INDEX idx_release_title_sort ON furumusic__release (title_sort)",
            // release: filter by type
            "CREATE INDEX idx_release_type ON furumusic__release (release_type)",

            // release_artist: unique pair + lookup
            "CREATE UNIQUE INDEX idx_release_artist_uniq ON furumusic__release_artist (release_id, artist_id)",
            "CREATE INDEX idx_release_artist_artist ON furumusic__release_artist (artist_id)",

            // track: search by normalized title
            "CREATE INDEX idx_track_title_sort ON furumusic__track (title_sort)",
            // track: FK to release
            "CREATE INDEX idx_track_release ON furumusic__track (release_id)",
            // track: FK to audio file
            "CREATE INDEX idx_track_audio_file ON furumusic__track (audio_file_id)",

            // track_artist: unique triple + lookups
            "CREATE UNIQUE INDEX idx_track_artist_uniq ON furumusic__track_artist (track_id, artist_id, role)",
            "CREATE INDEX idx_track_artist_artist ON furumusic__track_artist (artist_id)",

            // track_genre: unique pair + lookup
            "CREATE UNIQUE INDEX idx_track_genre_uniq ON furumusic__track_genre (track_id, genre_id)",
            "CREATE INDEX idx_track_genre_genre ON furumusic__track_genre (genre_id)",

            // genre: lookup by normalized name
            "CREATE INDEX idx_genre_normalized ON furumusic__genre (name_normalized)",

            // user_liked_track: unique pair + lookup by track
            "CREATE UNIQUE INDEX idx_user_liked_track_uniq ON furumusic__user_liked_track (user_id, track_id)",
            "CREATE INDEX idx_user_liked_track_track ON furumusic__user_liked_track (track_id)",

            // user_followed_artist: unique pair + lookup by artist
            "CREATE UNIQUE INDEX idx_user_followed_artist_uniq ON furumusic__user_followed_artist (user_id, artist_id)",
            "CREATE INDEX idx_user_followed_artist_artist ON furumusic__user_followed_artist (artist_id)",

            // playlist: owner lookup
            "CREATE INDEX idx_playlist_owner ON furumusic__playlist (owner_id)",

            // playlist_track: ordered tracks in playlist + lookup by track
            "CREATE INDEX idx_playlist_track_playlist ON furumusic__playlist_track (playlist_id, position)",
            "CREATE INDEX idx_playlist_track_track ON furumusic__playlist_track (track_id)",

            // saved_playlist: unique pair + lookup by playlist
            "CREATE UNIQUE INDEX idx_saved_playlist_uniq ON furumusic__saved_playlist (user_id, playlist_id)",
            "CREATE INDEX idx_saved_playlist_playlist ON furumusic__saved_playlist (playlist_id)",

            // play_history: user timeline + lookup by track
            "CREATE INDEX idx_play_history_user ON furumusic__play_history (user_id, played_at)",
            "CREATE INDEX idx_play_history_track ON furumusic__play_history (track_id)",

            // playback_state: one per user
            "CREATE UNIQUE INDEX idx_playback_state_user ON furumusic__playback_state (user_id)",

            // processing_task: queue polling (status + created_at)
            "CREATE INDEX idx_processing_task_status ON furumusic__processing_task (status, created_at)",
        ];

        for stmt in stmts {
            ctx.db.raw(stmt).await?;
        }

        Ok(())
    }

    #[derive(Debug, Copy, Clone)]
    pub struct M0019CreateMusicIndexes;

    impl migrations::Migration for M0019CreateMusicIndexes {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0019_create_music_indexes";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0018_create_processing_task",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::custom(create_music_indexes).build(),
        ];
    }

    // -- M0020: enable pg_trgm extension --------------------------------------

    #[cot::db::migrations::migration_op]
    async fn enable_pg_trgm(ctx: migrations::MigrationContext<'_>) -> cot::db::Result<()> {
        ctx.db.raw("CREATE EXTENSION IF NOT EXISTS pg_trgm").await?;
        Ok(())
    }

    #[derive(Debug, Copy, Clone)]
    pub struct M0020EnablePgTrgm;

    impl migrations::Migration for M0020EnablePgTrgm {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0020_enable_pg_trgm";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0019_create_music_indexes",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::custom(enable_pg_trgm).build(),
        ];
    }

    // -- M0021: GIN trigram indexes for fuzzy search --------------------------

    #[cot::db::migrations::migration_op]
    async fn create_trgm_indexes(ctx: migrations::MigrationContext<'_>) -> cot::db::Result<()> {
        ctx.db.raw("CREATE INDEX idx_artist_name_sort_trgm ON furumusic__artist USING gin (name_sort gin_trgm_ops)").await?;
        ctx.db.raw("CREATE INDEX idx_release_title_sort_trgm ON furumusic__release USING gin (title_sort gin_trgm_ops)").await?;
        Ok(())
    }

    #[derive(Debug, Copy, Clone)]
    pub struct M0021CreateTrgmIndexes;

    impl migrations::Migration for M0021CreateTrgmIndexes {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0021_create_trgm_indexes";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0020_enable_pg_trgm",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::custom(create_trgm_indexes).build(),
        ];
    }

    // -- M0022: GIN trigram index on track.title_sort ---------------------------

    #[cot::db::migrations::migration_op]
    async fn create_track_trgm_index(ctx: migrations::MigrationContext<'_>) -> cot::db::Result<()> {
        ctx.db.raw("CREATE INDEX IF NOT EXISTS idx_track_title_sort_trgm ON furumusic__track USING gin (title_sort gin_trgm_ops)").await?;
        Ok(())
    }

    #[derive(Debug, Copy, Clone)]
    pub struct M0022CreateTrackTrgmIndex;

    impl migrations::Migration for M0022CreateTrackTrgmIndex {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0022_create_track_trgm_index";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0021_create_trgm_indexes",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::custom(create_track_trgm_index).build(),
        ];
    }

    // -- M0028: add model_name to artist, release, track -----------------------

    #[cot::db::migrations::migration_op]
    async fn add_model_name_columns(ctx: migrations::MigrationContext<'_>) -> cot::db::Result<()> {
        ctx.db
            .raw("ALTER TABLE furumusic__artist ADD COLUMN model_name VARCHAR(128) DEFAULT NULL")
            .await?;
        ctx.db
            .raw("ALTER TABLE furumusic__release ADD COLUMN model_name VARCHAR(128) DEFAULT NULL")
            .await?;
        ctx.db
            .raw("ALTER TABLE furumusic__track ADD COLUMN model_name VARCHAR(128) DEFAULT NULL")
            .await?;
        Ok(())
    }

    #[derive(Debug, Copy, Clone)]
    pub struct M0028AddModelNameColumns;

    impl migrations::Migration for M0028AddModelNameColumns {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0028_add_model_name_columns";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0027_create_processing_stats",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::custom(add_model_name_columns).build(),
        ];
    }

    // -- M0029: add volume column to playback_state ----------------------------

    #[cot::db::migrations::migration_op]
    async fn add_playback_volume(ctx: migrations::MigrationContext<'_>) -> cot::db::Result<()> {
        ctx.db
            .raw("ALTER TABLE furumusic__playback_state ADD COLUMN volume DOUBLE PRECISION NOT NULL DEFAULT 0.7")
            .await?;
        Ok(())
    }

    #[derive(Debug, Copy, Clone)]
    pub struct M0029AddPlaybackVolume;

    impl migrations::Migration for M0029AddPlaybackVolume {
        const APP_NAME: &'static str = "furumusic";
        const MIGRATION_NAME: &'static str = "m_0029_add_playback_volume";
        const DEPENDENCIES: &'static [migrations::MigrationDependency] = &[
            migrations::MigrationDependency::migration(
                "furumusic",
                "m_0028_add_model_name_columns",
            ),
        ];
        const OPERATIONS: &'static [Operation] = &[
            Operation::custom(add_playback_volume).build(),
        ];
    }

    pub const MIGRATIONS: &[&SyncDynMigration] = &[
        &M0006CreateMediaFile,
        &M0007CreateArtist,
        &M0008CreateRelease,
        &M0009CreateReleaseArtist,
        &M0010CreateTrack,
        &M0011CreateTrackArtist,
        &M0012CreateGenreTables,
        &M0013CreateUserLikedTrack,
        &M0014CreateUserFollowedArtist,
        &M0015CreatePlaylistTables,
        &M0016CreatePlayHistory,
        &M0017CreatePlaybackState,
        &M0018CreateProcessingTask,
        &M0019CreateMusicIndexes,
        &M0020EnablePgTrgm,
        &M0021CreateTrgmIndexes,
        &M0022CreateTrackTrgmIndex,
        &M0028AddModelNameColumns,
        &M0029AddPlaybackVolume,
    ];
}
