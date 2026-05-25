use std::path::Path;

use symphonia::core::{
    codecs::CODEC_TYPE_NULL,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::{MetadataOptions, StandardTagKey},
    probe::Hint,
};

use super::dto::RawMetadata;

/// Extract metadata from an audio file.
///
/// For MP3, falls back to the `id3` crate when Symphonia cannot probe the file
/// (e.g. ID3 tag with large embedded cover art exceeds Symphonia's probe limit).
///
/// Must be called from a blocking context (`spawn_blocking`).
pub fn extract(path: &Path) -> anyhow::Result<RawMetadata> {
    match extract_via_symphonia(path) {
        Ok(mut meta) => {
            fill_average_bitrate(path, &mut meta);
            Ok(meta)
        }
        Err(e) => {
            let is_mp3 = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mp3"))
                .unwrap_or(false);
            if is_mp3 {
                tracing::debug!(error = %e, "Symphonia failed on MP3, falling back to id3 crate");
                let mut meta = extract_mp3_via_id3(path)?;
                fill_average_bitrate(path, &mut meta);
                Ok(meta)
            } else {
                Err(e)
            }
        }
    }
}

fn fill_average_bitrate(path: &Path, meta: &mut RawMetadata) {
    if meta.audio_bitrate.is_some() {
        return;
    }
    let Some(duration_secs) = meta.duration_secs.filter(|duration| *duration > 0.0) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let kbps = ((metadata.len() as f64 * 8.0) / duration_secs / 1000.0).round();
    if kbps.is_finite() && kbps > 0.0 && kbps <= i32::MAX as f64 {
        meta.audio_bitrate = Some(kbps as i32);
    }
}

fn extract_via_symphonia(path: &Path) -> anyhow::Result<RawMetadata> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions {
            enable_gapless: false,
            ..Default::default()
        },
        &MetadataOptions::default(),
    )?;

    let mut meta = RawMetadata::default();

    // Check metadata side-data (e.g. ID3 tags probed before format)
    if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        extract_tags(rev.tags(), &mut meta);
    }

    // Also check format-embedded metadata
    if let Some(rev) = probed.format.metadata().current() {
        if meta.title.is_none() {
            extract_tags(rev.tags(), &mut meta);
        }
    }

    let audio_track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL);

    if let Some(track) = audio_track {
        let params = &track.codec_params;
        meta.duration_secs = params.n_frames.and_then(|n_frames| {
            let tb = params.time_base?;
            Some(n_frames as f64 * tb.numer as f64 / tb.denom as f64)
        });
        meta.audio_sample_rate = params.sample_rate.and_then(|rate| i32::try_from(rate).ok());
        meta.audio_bit_depth = params
            .bits_per_sample
            .or(params.bits_per_coded_sample)
            .and_then(|bits| i32::try_from(bits).ok());
    }

    Ok(meta)
}

/// Read MP3 tags via the `id3` crate. Duration is not available this way.
fn extract_mp3_via_id3(path: &Path) -> anyhow::Result<RawMetadata> {
    use id3::TagLike;

    let tag =
        id3::Tag::read_from_path(path).map_err(|e| anyhow::anyhow!("id3 read failed: {}", e))?;

    let mut meta = RawMetadata::default();
    meta.title = tag.title().map(|s| fix_encoding(s.to_owned()));
    meta.artist = tag.artist().map(|s| fix_encoding(s.to_owned()));
    meta.album = tag.album().map(|s| fix_encoding(s.to_owned()));
    meta.year = tag.year().and_then(|y| u32::try_from(y).ok());
    meta.track_number = tag.track();
    meta.genre = tag.genre().map(|s: &str| fix_encoding(s.to_owned()));

    Ok(meta)
}

fn extract_tags(tags: &[symphonia::core::meta::Tag], meta: &mut RawMetadata) {
    for tag in tags {
        let value = fix_encoding(tag.value.to_string());
        if let Some(key) = tag.std_key {
            match key {
                StandardTagKey::TrackTitle => {
                    if meta.title.is_none() {
                        meta.title = Some(value);
                    }
                }
                StandardTagKey::Artist | StandardTagKey::Performer => {
                    if meta.artist.is_none() {
                        meta.artist = Some(value);
                    }
                }
                StandardTagKey::Album => {
                    if meta.album.is_none() {
                        meta.album = Some(value);
                    }
                }
                StandardTagKey::TrackNumber => {
                    if meta.track_number.is_none() {
                        meta.track_number = value.parse().ok();
                    }
                }
                StandardTagKey::Date | StandardTagKey::OriginalDate => {
                    if meta.year.is_none() {
                        let year_prefix: String = value.chars().take(4).collect();
                        meta.year = year_prefix.parse().ok();
                    }
                }
                StandardTagKey::Genre => {
                    if meta.genre.is_none() {
                        meta.genre = Some(value);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Heuristic to fix mojibake (CP1251 bytes interpreted as Latin-1/Windows-1252).
fn fix_encoding(s: String) -> String {
    let bytes: Vec<u8> = s
        .chars()
        .map(|c| c as u32)
        .filter(|&c| c <= 255)
        .map(|c| c as u8)
        .collect();

    if bytes.len() != s.chars().count() {
        return s;
    }

    let has_mojibake = bytes.iter().any(|&b| b >= 0xC0);
    if !has_mojibake {
        return s;
    }

    let (decoded, _, errors) = encoding_rs::WINDOWS_1251.decode(&bytes);
    if errors {
        return s;
    }

    decoded.into_owned()
}
