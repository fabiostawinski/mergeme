use chrono::{Local, TimeZone};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "avi", "mkv"];
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "heic", "heif", "webp"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Photo,
}

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The kind of media `path` is, based on its extension, or `None` if it's
/// neither a recognized video nor image extension.
pub fn media_kind(path: &Path) -> Option<MediaKind> {
    if has_extension(path, VIDEO_EXTENSIONS) {
        Some(MediaKind::Video)
    } else if has_extension(path, IMAGE_EXTENSIONS) {
        Some(MediaKind::Photo)
    } else {
        None
    }
}

/// Recursively finds video and picture files under `dir`, skipping macOS
/// junk files and Takeout sidecar JSON/HTML files.
pub fn scan_media(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    collect(dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// Whether `path` is a playable video ffprobe can read a duration from.
/// Catches truncated/corrupt files (e.g. a recording cut short mid-write,
/// leaving an MP4 with no moov atom) before they abort a batch encode.
pub fn is_readable_video(path: &Path) -> bool {
    Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration"])
        .arg(path)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Whether `path` is a picture ffprobe can decode a video frame from.
/// Catches truncated/corrupt images before they abort a batch encode.
pub fn is_readable_image(path: &Path) -> bool {
    Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width"])
        .arg(path)
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            collect(&p, out)?;
        } else if p.is_file() && !name.starts_with("._") && name != ".DS_Store" && media_kind(&p).is_some() {
            out.push(p);
        }
    }
    Ok(())
}

fn extract_json_number(body: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\"");
    let i = body.find(&pat)? + pat.len();
    let rest = &body[i..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    let digits: String = after_colon.chars().skip_while(|c| *c == '"').take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Like `extract_json_number`, but scoped to the first `outer_key` object in
/// `body` — Takeout's sidecar JSON has multiple `timestamp` fields
/// (`creationTime`, `photoTakenTime`, ...), so searching for the key name
/// alone finds whichever one happens to come first in the file, not
/// necessarily the one intended.
fn extract_nested_json_number(body: &str, outer_key: &str, inner_key: &str) -> Option<u64> {
    let outer_pos = body.find(&format!("\"{outer_key}\""))?;
    extract_json_number(&body[outer_pos..], inner_key)
}

/// Google Takeout writes a sidecar JSON per media file with the true capture
/// time in `photoTakenTime.timestamp` (epoch seconds) — more reliable than
/// embedded EXIF, which Takeout sometimes strips or rewrites. Not
/// `creationTime.timestamp`, which is when the file was uploaded/backed up
/// to Google Photos and can be much later than capture (e.g. a bulk backup).
fn takeout_sidecar_time(path: &Path) -> Option<u64> {
    let fname = path.file_name()?.to_str()?;
    let dir = path.parent()?;
    for candidate in [format!("{fname}.json"), format!("{fname}.supplemental-metadata.json")] {
        let sidecar = dir.join(&candidate);
        if let Ok(body) = fs::read_to_string(&sidecar) {
            if let Some(secs) = extract_nested_json_number(&body, "photoTakenTime", "timestamp") {
                return Some(secs);
            }
        }
    }
    None
}

fn exiftool_time(path: &Path) -> Option<u64> {
    let out = Command::new("exiftool")
        .arg("-CreateDate")
        .arg("-MediaCreateDate")
        .arg("-DateTimeOriginal")
        .arg("-T")
        .arg("-d")
        .arg("%s")
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .find_map(|tok| tok.parse::<u64>().ok())
}

/// Best-known capture time for a video: Takeout sidecar JSON, then EXIF
/// via `exiftool`, then falling back to filesystem mtime.
pub fn capture_time_secs(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    if let Some(secs) = takeout_sidecar_time(path) {
        return Ok(secs);
    }
    if let Some(secs) = exiftool_time(path) {
        return Ok(secs);
    }
    Ok(fs::metadata(path)?.modified()?.duration_since(UNIX_EPOCH)?.as_secs())
}

/// Stable "YYYY-MM" key for detecting month boundaries.
pub fn month_key(secs: u64) -> Result<String, Box<dyn std::error::Error>> {
    date_format(secs, "%Y-%m")
}

/// Human-readable "Month Year" label, e.g. "August 2026".
pub fn month_label(secs: u64) -> Result<String, Box<dyn std::error::Error>> {
    date_format(secs, "%B %Y")
}

/// "DD/MM/YYYY HH:MM" caption for a single clip, e.g. "01/08/2026 14:45".
pub fn day_time_label(secs: u64) -> Result<String, Box<dyn std::error::Error>> {
    date_format(secs, "%d/%m/%Y %H:%M")
}

fn date_format(secs: u64, fmt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let timestamp = i64::try_from(secs).map_err(|_| "timestamp is out of range")?;
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|date| date.format(fmt).to_string())
        .ok_or_else(|| "timestamp is not a valid local date".into())
}

#[derive(Debug, Clone)]
pub struct MediaItem {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub capture_secs: u64,
    pub size_bytes: u64,
    pub month_key: String,
    pub month_label: String,
    pub day_time_label: String,
}

pub fn load_items(paths: &[PathBuf]) -> Result<Vec<MediaItem>, Box<dyn std::error::Error>> {
    let mut items = Vec::with_capacity(paths.len());
    for path in paths {
        let capture_secs = capture_time_secs(path)?;
        let size_bytes = fs::metadata(path)?.len();
        let kind = media_kind(path).ok_or_else(|| format!("unrecognized media extension: {path:?}"))?;
        items.push(MediaItem {
            path: path.clone(),
            kind,
            capture_secs,
            size_bytes,
            month_key: month_key(capture_secs)?,
            month_label: month_label(capture_secs)?,
            day_time_label: day_time_label(capture_secs)?,
        });
    }
    items.sort_by_key(|i| i.capture_secs);
    Ok(items)
}

/// Indices (into `items`) where a new month begins, in order, including index 0.
pub fn month_boundaries(items: &[MediaItem]) -> Vec<usize> {
    let mut boundaries = Vec::new();
    let mut last_month: Option<&str> = None;
    for (i, item) in items.iter().enumerate() {
        if last_month != Some(item.month_key.as_str()) {
            boundaries.push(i);
            last_month = Some(&item.month_key);
        }
    }
    boundaries
}

/// Splits `items` into contiguous, chronologically-ordered batches of at most
/// `max_bytes` each; a single item larger than `max_bytes` becomes its own batch.
pub fn group_into_batches(items: &[MediaItem], max_bytes: u64) -> Vec<Vec<usize>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes: u64 = 0;
    for (i, item) in items.iter().enumerate() {
        if !current.is_empty() && current_bytes + item.size_bytes > max_bytes {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(i);
        current_bytes += item.size_bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(capture_secs: u64, size_bytes: u64, month_key: &str) -> MediaItem {
        MediaItem {
            path: PathBuf::from("x.mp4"),
            kind: MediaKind::Video,
            capture_secs,
            size_bytes,
            month_key: month_key.to_string(),
            month_label: month_key.to_string(),
            day_time_label: String::new(),
        }
    }

    #[test]
    fn day_time_label_matches_dd_mm_yyyy_hh_mm() {
        let label = day_time_label(1785692700).unwrap();
        let bytes = label.as_bytes();
        let is_digit = |i: usize| bytes[i].is_ascii_digit();
        assert_eq!(label.len(), 16, "expected DD/MM/YYYY HH:MM, got {label:?}");
        assert!(is_digit(0) && is_digit(1) && label.as_bytes()[2] == b'/');
        assert!(is_digit(3) && is_digit(4) && label.as_bytes()[5] == b'/');
        assert!((6..10).all(is_digit) && label.as_bytes()[10] == b' ');
        assert!(is_digit(11) && is_digit(12) && label.as_bytes()[13] == b':');
        assert!(is_digit(14) && is_digit(15));
    }

    #[test]
    fn month_boundaries_marks_first_item_and_each_transition() {
        let items = vec![item(1, 1, "2026-01"), item(2, 1, "2026-01"), item(3, 1, "2026-02"), item(4, 1, "2026-02")];
        assert_eq!(month_boundaries(&items), vec![0, 2]);
    }

    #[test]
    fn group_into_batches_respects_size_threshold() {
        let items = vec![item(1, 40, "2026-01"), item(2, 40, "2026-01"), item(3, 40, "2026-01"), item(4, 40, "2026-01")];
        let batches = group_into_batches(&items, 100);
        assert_eq!(batches, vec![vec![0, 1], vec![2, 3]]);
    }

    #[test]
    fn group_into_batches_oversized_item_gets_its_own_batch() {
        let items = vec![item(1, 10, "2026-01"), item(2, 500, "2026-01"), item(3, 10, "2026-01")];
        let batches = group_into_batches(&items, 100);
        assert_eq!(batches, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn extract_json_number_reads_quoted_timestamp() {
        let body = r#"{"photoTakenTime":{"timestamp":"1734000000","formatted":"x"}}"#;
        assert_eq!(extract_json_number(body, "timestamp"), Some(1734000000));
    }

    #[test]
    fn extract_nested_json_number_prefers_scoped_key_over_earlier_match() {
        let body = r#"{"creationTime":{"timestamp":"1999999999","formatted":"x"},"photoTakenTime":{"timestamp":"1734000000","formatted":"y"}}"#;
        assert_eq!(extract_nested_json_number(body, "photoTakenTime", "timestamp"), Some(1734000000));
    }
}
