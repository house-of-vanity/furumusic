use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;

#[derive(Debug, Clone, Copy)]
pub struct CoverVariant {
    pub name: &'static str,
    pub max_edge: u32,
    pub quality: u8,
}

pub const COVER_VARIANTS: &[CoverVariant] = &[
    CoverVariant {
        name: "small",
        max_edge: 96,
        quality: 80,
    },
    CoverVariant {
        name: "medium",
        max_edge: 256,
        quality: 82,
    },
    CoverVariant {
        name: "large",
        max_edge: 512,
        quality: 85,
    },
];

pub fn variant_by_name(name: &str) -> Option<CoverVariant> {
    COVER_VARIANTS
        .iter()
        .copied()
        .find(|variant| variant.name == name)
}

pub fn variant_path(original_path: &Path, variant: CoverVariant) -> PathBuf {
    let stem = original_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("cover");
    let filename = format!("{stem}.{}.jpg", variant.name);
    original_path.with_file_name(filename)
}

pub fn missing_variants(original_path: &Path) -> Vec<CoverVariant> {
    COVER_VARIANTS
        .iter()
        .copied()
        .filter(|variant| !variant_path(original_path, *variant).exists())
        .collect()
}

pub async fn ensure_cover_variants(original_path: &Path) -> anyhow::Result<usize> {
    let missing = missing_variants(original_path);
    if missing.is_empty() {
        return Ok(0);
    }

    let original_path = original_path.to_path_buf();
    tokio::task::spawn_blocking(move || generate_missing_variants_sync(&original_path, &missing))
        .await
        .map_err(|err| anyhow::anyhow!("cover variant task failed: {err}"))?
}

fn generate_missing_variants_sync(
    original_path: &Path,
    variants: &[CoverVariant],
) -> anyhow::Result<usize> {
    let data = std::fs::read(original_path)?;
    let image = image::load_from_memory(&data)?;

    let mut created = 0usize;
    for variant in variants {
        let path = variant_path(original_path, *variant);
        if path.exists() {
            continue;
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let resized = image
            .resize(variant.max_edge, variant.max_edge, FilterType::Lanczos3)
            .to_rgb8();
        let mut output = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut output, variant.quality);
        encoder.encode(
            &resized,
            resized.width(),
            resized.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        std::fs::write(path, output)?;
        created += 1;
    }

    Ok(created)
}
