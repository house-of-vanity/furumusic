pub mod artwork_backfill;
pub mod inbox_discover;
pub mod inbox_process;
pub mod lastfm_popularity;
pub mod lastfm_scrobble;
pub mod metadata_backfill;
pub mod musicbrainz;

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct UploaderAttribution {
    pub user_id: Option<i64>,
    pub name: String,
}

impl UploaderAttribution {
    pub fn unknown() -> Self {
        Self {
            user_id: None,
            name: "UFO".to_string(),
        }
    }
}

pub fn strip_user_upload_prefix(relative_path: &Path) -> PathBuf {
    let components: Vec<_> = relative_path.components().collect();
    if components.len() >= 3
        && matches!(components[0], Component::Normal(value) if value == "user_uploads")
    {
        components[2..].iter().collect()
    } else {
        relative_path.to_path_buf()
    }
}

pub async fn uploader_from_relative_path(
    pool: &sqlx::PgPool,
    relative_path: &Path,
) -> UploaderAttribution {
    let components: Vec<_> = relative_path.components().collect();
    let Some(Component::Normal(root)) = components.first() else {
        return UploaderAttribution::unknown();
    };
    if *root != "user_uploads" {
        return UploaderAttribution::unknown();
    }

    let Some(Component::Normal(user_id_os)) = components.get(1) else {
        return UploaderAttribution::unknown();
    };
    let Some(user_id_str) = user_id_os.to_str() else {
        return UploaderAttribution::unknown();
    };
    let Ok(user_id) = user_id_str.parse::<i64>() else {
        return UploaderAttribution::unknown();
    };

    let name: Option<String> = sqlx::query_scalar(
        r#"SELECT COALESCE(NULLIF(display_name, ''), username)::text
           FROM furumusic__user
           WHERE id = $1 AND is_active = true"#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    match name {
        Some(name) if !name.trim().is_empty() => UploaderAttribution {
            user_id: Some(user_id),
            name,
        },
        _ => UploaderAttribution::unknown(),
    }
}
