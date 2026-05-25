use serde::{Deserialize, Serialize};

/// Raw metadata extracted from audio file tags.
#[derive(Debug, Default)]
pub struct RawMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u32>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub duration_secs: Option<f64>,
    pub audio_bitrate: Option<i32>,
    pub audio_sample_rate: Option<i32>,
    pub audio_bit_depth: Option<i32>,
}

/// Hints parsed from the file path (directory structure + filename).
#[derive(Debug, Default)]
pub struct PathHints {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<i32>,
}

/// Normalized metadata returned by the LLM.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NormalizedFields {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<i32>,
    pub genre: Option<String>,
    #[serde(default)]
    pub featured_artists: Vec<String>,
    pub release_type: Option<String>,
    pub confidence: Option<f64>,
    pub notes: Option<String>,
}

/// A similar artist found via pg_trgm fuzzy search.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SimilarArtist {
    pub id: i64,
    pub name: String,
    pub similarity: f32,
}

/// A similar release found via pg_trgm fuzzy search.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SimilarRelease {
    pub id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub similarity: f32,
}

/// Context about other files in the same folder (for the LLM).
pub struct FolderContext {
    pub folder_path: String,
    pub folder_files: Vec<String>,
    pub track_count: usize,
}
