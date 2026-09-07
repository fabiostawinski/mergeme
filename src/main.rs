use mergeme::{
    group_into_batches, is_readable_image, is_readable_video, load_items, media_kind, month_boundaries, scan_media,
    MediaItem, MediaKind,
};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;

const ONE_GIB: u64 = 1024 * 1024 * 1024;
const TARGET_WIDTH: u32 = 1920;
const TARGET_HEIGHT: u32 = 1080;
const TARGET_FPS: u32 = 30;
const CRF: &str = "20";
const PRESET: &str = "medium";
const BUMPER_SECONDS: u32 = 5;
// The title overlay spans the same duration as the opening month bumper it
// appears over, so it doesn't disappear while that bumper is still showing.
const TITLE_SECONDS: u32 = BUMPER_SECONDS;
const DEFAULT_PHOTO_SECONDS: f64 = 2.0;
// Intermediate segments use PCM audio, not AAC: AAC's per-encode priming
// delay accumulates across the hundreds of independently-encoded segments a
// photo-heavy input produces, silently padding the concatenated audio well
// beyond its nominal duration (measured: ~13s of phantom padding over 400
// half-second segments) and confusing downstream filters like `amix`. Only
// the final output (re-encoded once, not concatenated) uses AAC.
const INTERMEDIATE_AUDIO_CODEC: &str = "pcm_s16le";
// Also how long the background song takes to fade out under it, so the
// music finishes silent right as the video ends.
const OUTRO_SECONDS: u32 = 3;

struct Args {
    input: PathBuf,
    output: PathBuf,
    title: String,
    song: PathBuf,
    song_volume: f64,
    work_dir: PathBuf,
    jobs: usize,
    dry_run: bool,
    photo_seconds: f64,
    no_overlays: bool,
    /// Manual rotation overrides, keyed by filename (e.g. "clip.MP4") or full
    /// path, for clips whose camera didn't embed rotation metadata (degrees
    /// clockwise needed to correct the picture: 90, 180, or 270).
    rotate_overrides: HashMap<String, i32>,
    /// Manual bumper-card insertions, keyed by filename or full path, mapping
    /// to the label text to show on a bumper card inserted right before that
    /// item. Applied whether or not `--no-overlays` disabled the automatic
    /// month markers, and overrides an automatic marker's label if the item
    /// already got one.
    marker_overrides: HashMap<String, String>,
    /// A `--marker` given as plain text (no `<name>=`), inserted as a bumper
    /// card before the very first item, overriding whatever else landed
    /// there (an automatic month marker or a filename-keyed `--marker`).
    intro_marker: Option<String>,
}

fn print_usage() {
    eprintln!(
        "Usage: mergeme --input <dir> --output <file> --title <text> --song <audio file> \
         [--song-volume <0.0-1.0>] [--work-dir <dir>] [--jobs <n>] [--dry-run] \
         [--photo-seconds <n>] [--no-overlays] [--rotate <filename-or-path>=<90|180|270> ...] \
         [--marker <filename-or-path>=<label text> | <label text> ...]"
    );
}

fn parse_args() -> Result<Args, Box<dyn std::error::Error>> {
    let mut input = None;
    let mut output = None;
    let mut title = None;
    let mut song = None;
    let mut song_volume = 0.15;
    let mut work_dir = None;
    let mut jobs = thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4);
    let mut dry_run = false;
    let mut photo_seconds = DEFAULT_PHOTO_SECONDS;
    let mut no_overlays = false;
    let mut rotate_overrides = HashMap::new();
    let mut marker_overrides = HashMap::new();
    let mut intro_marker = None;

    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--title" => title = args.next(),
            "--song" => song = args.next().map(PathBuf::from),
            "--song-volume" => song_volume = args.next().ok_or("missing value for --song-volume")?.parse()?,
            "--work-dir" => work_dir = args.next().map(PathBuf::from),
            "--jobs" => jobs = args.next().ok_or("missing value for --jobs")?.parse()?,
            "--dry-run" => dry_run = true,
            "--photo-seconds" => photo_seconds = args.next().ok_or("missing value for --photo-seconds")?.parse()?,
            "--no-overlays" => no_overlays = true,
            "--rotate" => {
                let spec = args.next().ok_or("missing value for --rotate (expected <name>=<degrees>)")?;
                let (name, deg) = spec.split_once('=').ok_or("--rotate expects <name>=<degrees>")?;
                let deg: i32 = deg.parse()?;
                if ![90, 180, 270].contains(&deg) {
                    return Err(format!("--rotate degrees must be 90, 180, or 270, got {deg}").into());
                }
                rotate_overrides.insert(name.to_string(), deg);
            }
            "--marker" => {
                let spec = args.next().ok_or("missing value for --marker (expected <label> or <name>=<label>)")?;
                match spec.split_once('=') {
                    Some((name, label)) => {
                        marker_overrides.insert(name.to_string(), label.to_string());
                    }
                    None => intro_marker = Some(spec),
                }
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            _ => {
                eprintln!("Unknown arg: {a}");
                print_usage();
                std::process::exit(1);
            }
        }
    }

    let input = input.ok_or("--input is required")?;
    let output = output.ok_or("--output is required")?;
    let title = title.ok_or("--title is required")?;
    let song = song.ok_or("--song is required")?;
    let work_dir = work_dir.unwrap_or_else(|| {
        let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("mergeme");
        output.parent().unwrap_or(Path::new(".")).join(format!(".{stem}-work"))
    });

    Ok(Args {
        input,
        output,
        title,
        song,
        song_volume,
        work_dir,
        jobs: jobs.max(1),
        dry_run,
        photo_seconds,
        no_overlays,
        rotate_overrides,
        marker_overrides,
        intro_marker,
    })
}

fn run_checked(mut cmd: Command, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(format!("{label} failed:\n{}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(())
}

/// Every output the pipeline caches by "does this file exist" must never end
/// up looking complete after an interrupted run (killed process, Ctrl-C,
/// crash mid-write) — a later run's cache check can't tell a corrupt partial
/// file from a finished one, and will silently reuse it. `cmd` must be
/// configured to write to `tmp_path_for(final_path)`; this runs it and, only
/// on success, atomically renames the temp file into place.
fn tmp_path_for(final_path: &Path) -> PathBuf {
    // The suffix goes before the extension, not after, so ffmpeg's output
    // muxer is still auto-detected correctly from a recognized extension.
    let ext = final_path.extension().and_then(|e| e.to_str()).unwrap_or("tmp");
    let stem = final_path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    final_path.with_file_name(format!("{stem}.partial.{ext}"))
}

fn run_checked_atomic(cmd: Command, final_path: &Path, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    run_checked(cmd, label)?;
    fs::rename(tmp_path_for(final_path), final_path)?;
    Ok(())
}

fn escape_drawtext(s: &str) -> String {
    s.replace('\\', "\\\\").replace(':', "\\:").replace('\'', "\\'").replace('%', "\\%")
}

/// A real font file on disk for `drawtext` to use: first asks fontconfig
/// (`fc-match`), then falls back to hardcoded paths for common installs on
/// this OS. See `fontfile_fragment` for why this is needed on every
/// platform, not just Linux.
fn detect_font_path() -> Option<String> {
    let from_fontconfig = Command::new("fc-match")
        .args(["-f", "%{file}", "sans-serif:bold"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|path| !path.is_empty() && Path::new(path).exists());
    if from_fontconfig.is_some() {
        return from_fontconfig;
    }

    let candidates: &[&str] = if cfg!(target_os = "windows") {
        // Forward slashes even on Windows: ffmpeg's filter-string escaping
        // only needs to worry about the drive-letter colon then, not every
        // path separator too.
        &[
            "C:/Windows/Fonts/arialbd.ttf",
            "C:/Windows/Fonts/segoeuib.ttf",
            "C:/Windows/Fonts/calibrib.ttf",
            "C:/Windows/Fonts/arial.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/Library/Fonts/Arial Bold.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        ]
    };
    candidates.iter().map(|p| p.to_string()).find(|p| Path::new(p).exists())
}

/// A `fontfile='...':` drawtext fragment (trailing colon included) to
/// prepend to every `drawtext=` filter, or empty if no font could be found.
///
/// `drawtext` with no `fontfile` asks fontconfig to match a default family,
/// which needs fontconfig to be both compiled in *and* configured with a
/// `fonts.conf` it can load. That combination can't be taken for granted
/// anywhere: confirmed in practice on Windows with the gyan.dev full ffmpeg
/// build, which enables fontconfig but ships no config for it, failing with
/// "Fontconfig error: Cannot load default config file: File not found";
/// Linux is at least as exposed, since static builds there often skip
/// fontconfig entirely and minimal/headless images frequently have no fonts
/// installed even when it's present. Passing an explicit `fontfile` avoids
/// fontconfig entirely (it only needs libfreetype to open the file), which
/// is why it fixes the problem regardless of platform or root cause.
/// Computed once and cached: it shells out to `fc-match`, and this is called
/// once per encoded segment.
fn fontfile_fragment() -> &'static str {
    static FRAGMENT: OnceLock<String> = OnceLock::new();
    FRAGMENT.get_or_init(|| match detect_font_path() {
        Some(path) => format!("fontfile='{}':", escape_drawtext(&path)),
        None => String::new(),
    })
}

/// Fails fast, before any batch encoding starts, if text overlays are about
/// to fail for lack of a font. `--dry-run` skips this: it never invokes
/// ffmpeg, so a missing font there isn't fatal to what dry-run actually does.
fn ensure_font_available() -> Result<(), Box<dyn std::error::Error>> {
    if !fontfile_fragment().is_empty() {
        return Ok(());
    }
    let hint = if cfg!(target_os = "windows") {
        "checked fontconfig via `fc-match` and common fonts under C:\\Windows\\Fonts \
         (Arial, Segoe UI, Calibri)"
    } else if cfg!(target_os = "macos") {
        "checked fontconfig via `fc-match` and the usual system Arial/Helvetica locations"
    } else {
        "checked fontconfig via `fc-match` and common DejaVu/Liberation/Noto install paths; \
         install a font package, e.g. `sudo apt-get install fonts-dejavu-core` on \
         Debian/Ubuntu or `sudo dnf install dejavu-sans-fonts` on Fedora"
    };
    Err(format!("No usable font file found for burning in the title/date text ({hint}).").into())
}

/// Fails fast with a single clear message if `ffmpeg`, `ffprobe`, or
/// `exiftool` aren't on `PATH`, instead of letting a missing `ffprobe` sail
/// through silently: `is_readable_video`/`is_readable_image` treat a failed
/// probe as "unreadable" (see their doc comments), so a missing `ffprobe`
/// otherwise surfaces as every single file being skipped and a misleading
/// "no readable video or picture files found" error.
fn ensure_required_tools_available() -> Result<(), Box<dyn std::error::Error>> {
    let checks: &[(&str, &str)] = &[("ffmpeg", "-version"), ("ffprobe", "-version"), ("exiftool", "-ver")];
    let missing: Vec<&str> =
        checks.iter().filter(|(tool, arg)| Command::new(tool).arg(arg).output().is_err()).map(|(tool, _)| *tool).collect();
    if !missing.is_empty() {
        return Err(format!(
            "Required tool(s) not found on PATH: {}. Install them first (see the README's \
             Runtime requirements section, or run scripts/check-tools.sh / \
             scripts\\check-tools.ps1).",
            missing.join(", ")
        )
        .into());
    }
    Ok(())
}

fn ffprobe_duration(path: &Path) -> Result<f64, Box<dyn std::error::Error>> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse()?)
}

/// Whether `path` carries any rotation metadata (QuickTime `rotate` tag or a
/// track display matrix). Clips with metadata are already auto-corrected by
/// ffmpeg's default handling; clips with none are the only ones at risk of
/// landing on the target canvas sideways, since there's nothing for ffmpeg
/// to auto-correct. This can't tell you whether a metadata-less clip
/// actually needs `--rotate` — only that it's worth a quick look.
fn has_rotation_metadata(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream_tags=rotate:stream_side_data=rotation",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// A `transpose` filter chain (trailing comma included) that rotates the
/// picture `deg` degrees clockwise, or empty for 0/unrecognized values.
///
/// Only ever applied for explicit `--rotate` overrides. FFmpeg already
/// auto-applies a source's own rotation metadata (QuickTime `rotate` tag or
/// track display matrix) by default before any `-vf` chain runs, so clips
/// that carry that metadata must NOT also get a transpose here — that would
/// rotate them twice. `--rotate` exists only for clips with no rotation
/// metadata at all (observed on a handful of clips from one DJI camera),
/// which ffmpeg has nothing to auto-correct.
fn rotation_filter(deg: i32) -> &'static str {
    match deg {
        90 => "transpose=1,",
        180 => "transpose=1,transpose=1,",
        270 => "transpose=2,",
        _ => "",
    }
}

fn audio_channels(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=channels", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(format!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(2))
}

/// Whether `path` has any audio stream at all. Some action-cam/drone clips
/// (observed: DJI timelapses, GoPro hyperlapses) are recorded with no audio
/// track whatsoever — not silent audio, none. Without this check such a clip
/// would be normalized with no audio stream either, and the concat demuxer's
/// audio timeline would then skip straight over that clip's duration instead
/// of covering it with silence, desyncing every clip's audio after it.
fn has_audio_stream(path: &Path) -> bool {
    Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

fn generate_bumper(work_dir: &Path, name: &str, label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join(format!("{name}.mp4"));
    if out_path.exists() {
        return Ok(out_path);
    }
    let text = escape_drawtext(label);
    let fontfile = fontfile_fragment();
    let vf = format!(
        "drawtext={fontfile}text='{text}':fontsize=90:fontcolor=white:x=(w-text_w)/2:y=(h-text_h)/2"
    );
    let cmd_args = [
        "-y".to_string(),
        "-f".into(), "lavfi".into(),
        "-i".into(), format!("color=c=black:s={TARGET_WIDTH}x{TARGET_HEIGHT}:d={BUMPER_SECONDS}:r={TARGET_FPS}"),
        "-f".into(), "lavfi".into(),
        "-i".into(), "anullsrc=r=48000:cl=stereo".into(),
        "-t".into(), BUMPER_SECONDS.to_string(),
        "-vf".into(), vf,
        "-c:v".into(), "libx264".into(),
        "-crf".into(), CRF.into(),
        "-preset".into(), PRESET.into(),
        "-c:a".into(), INTERMEDIATE_AUDIO_CODEC.into(),
        tmp_path_for(&out_path).to_string_lossy().into_owned(),
    ];
    let mut cmd = Command::new("ffmpeg");
    cmd.args(&cmd_args);
    run_checked_atomic(cmd, &out_path, &format!("generating bumper {name}"))?;
    Ok(out_path)
}

/// A silent black clip, `OUTRO_SECONDS` long at the target spec, appended
/// after the last clip so the video ends on black instead of cutting
/// straight from the last frame; the background song fades out under it
/// (see the `afade` in `build_final`).
fn generate_outro(work_dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join("outro.mp4");
    if out_path.exists() {
        return Ok(out_path);
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-f", "lavfi", "-i", &format!("color=c=black:s={TARGET_WIDTH}x{TARGET_HEIGHT}:d={OUTRO_SECONDS}:r={TARGET_FPS}")]);
    cmd.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", &OUTRO_SECONDS.to_string()]);
    cmd.args(["-c:v", "libx264", "-crf", CRF, "-preset", PRESET, "-c:a", INTERMEDIATE_AUDIO_CODEC]);
    cmd.arg(tmp_path_for(&out_path));
    run_checked_atomic(cmd, &out_path, "generating outro")?;
    Ok(out_path)
}

/// Re-encodes a single input to the common target spec (resolution, fps,
/// audio format) in one single-input pass. Multi-input filter-graph concat
/// was tried first but FFmpeg's `concat` filter can silently drop most of a
/// segment's audio when frame rates differ across inputs (observed ~76%
/// silence within a 24fps segment concatenated after a 30fps one) — instead,
/// each segment is normalized independently here, then joined with the fast
/// and reliable concat *demuxer* once all segments share identical params.
fn normalize_segment(
    work_dir: &Path,
    name: &str,
    src: &Path,
    caption: Option<&str>,
    rotation_deg: i32,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join(format!("{name}.mp4"));
    if out_path.exists() {
        return Ok(out_path);
    }
    let vf = segment_vf(caption, rotation_deg);
    // CFR conversion quantizes video to a whole number of frames at
    // TARGET_FPS, very slightly different from the source's exact (sub
    // frame) duration; forcing every segment's audio AND video to this same
    // quantized length via an explicit -t (rather than trusting them to
    // naturally line up) is what keeps hundreds of concatenated segments
    // from drifting out of sync by the end of a long video. `apad`/anullsrc
    // guarantee there's enough audio available for -t to trim to that exact
    // length; plain `-shortest` was tried first but doesn't reliably cut a
    // padded/silent audio stream to match a simple-filtergraph video stream.
    let src_duration = ffprobe_duration(src)?;
    let target_duration = (src_duration * TARGET_FPS as f64).round() / TARGET_FPS as f64;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y").arg("-i").arg(src);

    if has_audio_stream(src) {
        // aformat's implicit mono->stereo upmix applies a loudness-law gain
        // (~-3dB); duplicating the channel explicitly first avoids that.
        let af = if audio_channels(src)? == 1 {
            "pan=stereo|c0=c0|c1=c0,aformat=sample_rates=48000:channel_layouts=stereo,apad".to_string()
        } else {
            "aformat=sample_rates=48000:channel_layouts=stereo,apad".to_string()
        };
        cmd.arg("-vf").arg(&vf).args(["-r", &TARGET_FPS.to_string(), "-fps_mode", "cfr"]);
        cmd.arg("-af").arg(&af);
    } else {
        // No audio track at all: feed a silent one so every segment has
        // audio and the concat demuxer's audio timeline still covers this
        // clip's duration.
        cmd.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"]);
        cmd.arg("-vf").arg(&vf).args(["-r", &TARGET_FPS.to_string(), "-fps_mode", "cfr"]);
        cmd.args(["-map", "0:v", "-map", "1:a"]);
    }
    cmd.args(["-t", &target_duration.to_string()]);

    cmd.args(["-c:v", "libx264", "-crf", CRF, "-preset", PRESET, "-c:a", INTERMEDIATE_AUDIO_CODEC]);
    cmd.arg(tmp_path_for(&out_path));
    run_checked_atomic(cmd, &out_path, &format!("normalizing {name}"))?;
    Ok(out_path)
}

/// Turns a still picture into a silent `photo_seconds`-long clip at the
/// common target spec, so it can be concatenated alongside video segments.
fn normalize_photo(
    work_dir: &Path,
    name: &str,
    src: &Path,
    caption: Option<&str>,
    rotation_deg: i32,
    photo_seconds: f64,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join(format!("{name}.mp4"));
    if out_path.exists() {
        return Ok(out_path);
    }
    let vf = segment_vf(caption, rotation_deg);
    let duration = photo_seconds.to_string();

    // `-loop 1` is an image2-demuxer-only input option, but ffmpeg detects
    // some still-picture formats (HEIC/HEIF observed in practice) as their
    // underlying ISO-BMFF container instead of image2, where `-loop` isn't a
    // recognized option and the whole command fails outright. Decoding to a
    // plain PNG frame first sidesteps the format detection entirely.
    let png_path = work_dir.join(format!("{name}_src.png"));
    let mut decode_cmd = Command::new("ffmpeg");
    decode_cmd.args(["-y", "-i"]).arg(src).args(["-frames:v", "1", "-update", "1"]).arg(&png_path);
    run_checked(decode_cmd, &format!("decoding {name}"))?;

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-loop", "1", "-t", &duration]).arg("-i").arg(&png_path);
    cmd.args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", &duration]);
    cmd.arg("-vf").arg(&vf).args(["-r", &TARGET_FPS.to_string(), "-fps_mode", "cfr"]);
    cmd.args(["-c:v", "libx264", "-crf", CRF, "-preset", PRESET, "-c:a", INTERMEDIATE_AUDIO_CODEC]);
    cmd.arg(tmp_path_for(&out_path));
    run_checked_atomic(cmd, &out_path, &format!("normalizing {name}"))?;
    Ok(out_path)
}

/// The scale/pad/caption filter chain shared by video segments and photo
/// segments, so both land on the identical target spec the concat demuxer
/// requires. `caption` is `None` when `--no-overlays` suppresses the
/// burned-in date/time label.
fn segment_vf(caption: Option<&str>, rotation_deg: i32) -> String {
    let rotate = rotation_filter(rotation_deg);
    let base = format!(
        "{rotate}scale={TARGET_WIDTH}:{TARGET_HEIGHT}:force_original_aspect_ratio=decrease,\
         pad={TARGET_WIDTH}:{TARGET_HEIGHT}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={TARGET_FPS}"
    );
    match caption {
        Some(caption) => {
            let caption_text = escape_drawtext(caption);
            let fontfile = fontfile_fragment();
            format!(
                "{base},drawtext={fontfile}text='{caption_text}':fontsize=26:fontcolor=white@0.9:box=1:boxcolor=black@0.4:\
                 boxborderw=6:x=w-text_w-24:y=h-text_h-24"
            )
        }
        None => base,
    }
}

fn build_batch(
    work_dir: &Path,
    batch_number: usize,
    items: &[MediaItem],
    indices: &[usize],
    markers: &HashMap<usize, String>,
    rotations: &[i32],
    photo_seconds: f64,
    show_overlays: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join(format!("batch_{batch_number:04}.mp4"));
    if out_path.exists() {
        return Ok(out_path);
    }

    let mut segments = Vec::new();
    for (pos, &idx) in indices.iter().enumerate() {
        if let Some(label) = markers.get(&idx) {
            let bumper_name = format!("bumper_b{batch_number}_p{pos}");
            // Bumpers are generated at the exact target spec already.
            segments.push(generate_bumper(work_dir, &bumper_name, label)?);
        }
        let norm_name = format!("norm_b{batch_number}_p{pos}");
        let item = &items[idx];
        let caption = show_overlays.then_some(item.day_time_label.as_str());
        segments.push(match item.kind {
            MediaKind::Video => normalize_segment(work_dir, &norm_name, &item.path, caption, rotations[idx])?,
            MediaKind::Photo => normalize_photo(work_dir, &norm_name, &item.path, caption, rotations[idx], photo_seconds)?,
        });
    }

    concat_demuxer(work_dir, &format!("batch_{batch_number:04}_list.txt"), &segments, &out_path)?;
    Ok(out_path)
}

fn build_batches_parallel(
    work_dir: &Path,
    items: &[MediaItem],
    batches: &[Vec<usize>],
    markers: &HashMap<usize, String>,
    rotations: &[i32],
    photo_seconds: f64,
    show_overlays: bool,
    jobs: usize,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let numbered: Vec<(usize, &Vec<usize>)> = batches.iter().enumerate().collect();
    let mut outputs = Vec::with_capacity(batches.len());
    for chunk in numbered.chunks(jobs) {
        let results: Vec<Result<PathBuf, Box<dyn std::error::Error + Send + Sync>>> = thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(batch_number, indices)| {
                    let batch_number = *batch_number;
                    let indices: &[usize] = indices;
                    scope.spawn(move || {
                        build_batch(work_dir, batch_number, items, indices, markers, rotations, photo_seconds, show_overlays)
                            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { format!("{e}").into() })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for r in results {
            outputs.push(r.map_err(|e| e.to_string())?);
        }
    }
    Ok(outputs)
}

/// Joins already-identically-encoded `segments` via the concat demuxer
/// (stream copy, no re-encode) into `out_path`.
fn concat_demuxer(work_dir: &Path, list_name: &str, segments: &[PathBuf], out_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let list_path = work_dir.join(list_name);
    let list_body: String =
        segments.iter().map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\'', "'\\''"))).collect();
    fs::write(&list_path, list_body)?;

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-f", "concat", "-safe", "0", "-i"]).arg(&list_path).args(["-c", "copy"]).arg(tmp_path_for(out_path));
    run_checked_atomic(cmd, out_path, &format!("concatenating into {out_path:?}"))
}

fn concat_batches(work_dir: &Path, batch_outputs: &[PathBuf]) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join("concat.mp4");
    concat_demuxer(work_dir, "concat_list.txt", batch_outputs, &out_path)?;
    Ok(out_path)
}

/// Mixes the original audio and the looped, fading background song down to
/// one AAC track. Kept in its own ffmpeg invocation — with no video encode
/// competing for the same process — because combining this audio filtergraph
/// with a simultaneous libx264 video encode was observed to silently drop
/// large chunks of the mixed audio (tens of seconds at a time) partway
/// through long videos, even though the same filtergraph alone is reliable.
fn mix_audio(
    work_dir: &Path,
    concat_path: &Path,
    song: &Path,
    duration: f64,
    fade_start: f64,
    fade_duration: f64,
    song_volume: f64,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_path = work_dir.join("mixed_audio.m4a");
    let filter = format!(
        "[1:a]aloop=loop=-1:size=2000000000,atrim=0:{duration},\
         afade=t=out:st={fade_start}:d={fade_duration},volume={song_volume}[song];\
         [0:a][song]amix=inputs=2:duration=first:dropout_transition=0[aout]"
    );

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y").arg("-i").arg(concat_path).arg("-i").arg(song);
    cmd.arg("-filter_complex").arg(&filter);
    cmd.args(["-map", "[aout]", "-c:a", "aac"]);
    cmd.arg(tmp_path_for(&out_path));
    run_checked_atomic(cmd, &out_path, "mixing audio")?;
    Ok(out_path)
}

fn build_final(
    work_dir: &Path,
    concat_path: &Path,
    song: &Path,
    title: &str,
    song_volume: f64,
    out_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let duration = ffprobe_duration(concat_path)?;
    // The outro clip appended before this call makes up the tail end of
    // `duration`, so the fade lands exactly under the black screen.
    let fade_duration = (OUTRO_SECONDS as f64).min(duration);
    let fade_start = duration - fade_duration;
    let mixed_audio = mix_audio(work_dir, concat_path, song, duration, fade_start, fade_duration, song_volume)?;

    let title_text = escape_drawtext(title);
    let fontfile = fontfile_fragment();
    let drawtext = format!(
        "drawtext={fontfile}text='{title_text}':enable='lt(t\\,{TITLE_SECONDS})':fontsize=64:fontcolor=white:\
         box=1:boxcolor=black@0.5:boxborderw=20:x=(w-text_w)/2:y=h-160[vout]"
    );

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y").arg("-i").arg(concat_path).arg("-i").arg(&mixed_audio);
    cmd.arg("-filter_complex").arg(format!("[0:v]{drawtext}"));
    cmd.args(["-map", "[vout]", "-map", "1:a"]);
    cmd.args(["-c:v", "libx264", "-crf", CRF, "-preset", PRESET, "-c:a", "copy"]);
    cmd.arg(tmp_path_for(out_path));
    run_checked_atomic(cmd, out_path, "final assembly")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    ensure_required_tools_available()?;

    // The default work-dir sits right next to the output file, which can
    // itself land inside the input folder (e.g. `--input photos --output
    // photos/out.mp4`); excluding it here keeps a rerun from rediscovering
    // its own previous run's intermediate segments/bumpers/decoded frames as
    // if they were fresh source media.
    let scanned: Vec<PathBuf> =
        scan_media(&args.input)?.into_iter().filter(|p| !p.starts_with(&args.work_dir)).collect();
    if scanned.is_empty() {
        return Err(format!("no video or picture files found under {:?}", args.input).into());
    }
    let mut paths = Vec::with_capacity(scanned.len());
    for path in scanned {
        let readable = match media_kind(&path) {
            Some(MediaKind::Video) => is_readable_video(&path),
            Some(MediaKind::Photo) => is_readable_image(&path),
            None => false,
        };
        if readable {
            paths.push(path);
        } else {
            eprintln!("Skipping unreadable/corrupt file: {path:?}");
        }
    }
    if paths.is_empty() {
        return Err("no readable video or picture files found".into());
    }
    let items = load_items(&paths)?;
    let rotations: Vec<i32> = items
        .iter()
        .map(|item| {
            let filename = item.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let full_path = item.path.to_string_lossy();
            args.rotate_overrides
                .get(filename)
                .or_else(|| args.rotate_overrides.get(full_path.as_ref()))
                .copied()
                .unwrap_or(0)
        })
        .collect();
    let unverified_rotation: Vec<&Path> = items
        .iter()
        .zip(&rotations)
        .filter(|(item, &deg)| deg == 0 && item.kind == MediaKind::Video)
        .filter_map(|(item, _)| match has_rotation_metadata(&item.path) {
            Ok(true) => None,
            Ok(false) => Some(item.path.as_path()),
            Err(e) => {
                eprintln!("Warning: could not check rotation metadata for {:?}: {e}", item.path);
                None
            }
        })
        .collect();
    if !unverified_rotation.is_empty() {
        eprintln!(
            "Warning: {} clip(s) have no rotation metadata, so ffmpeg has nothing to \
             auto-correct — most clips like this are still fine, but it's worth a quick \
             look in case one lands sideways. Pass --rotate <file>=<90|180|270> for any that do:",
            unverified_rotation.len()
        );
        for path in &unverified_rotation {
            eprintln!("  {path:?}");
        }
    }

    let mut markers: HashMap<usize, String> = if args.no_overlays {
        HashMap::new()
    } else {
        month_boundaries(&items).into_iter().map(|idx| (idx, items[idx].month_label.clone())).collect()
    };
    for (idx, item) in items.iter().enumerate() {
        let filename = item.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let full_path = item.path.to_string_lossy();
        if let Some(label) =
            args.marker_overrides.get(filename).or_else(|| args.marker_overrides.get(full_path.as_ref()))
        {
            markers.insert(idx, label.clone());
        }
    }
    if let Some(label) = &args.intro_marker {
        markers.insert(0, label.clone());
    }
    let batches = group_into_batches(&items, ONE_GIB);

    let total_bytes: u64 = items.iter().map(|i| i.size_bytes).sum();
    let photo_count = items.iter().filter(|i| i.kind == MediaKind::Photo).count();
    println!(
        "{} videos, {} picture(s), {:.1} GiB total, {} marker(s), {} batch(es)",
        items.len() - photo_count,
        photo_count,
        total_bytes as f64 / ONE_GIB as f64,
        markers.len(),
        batches.len()
    );
    let mut marker_indices: Vec<&usize> = markers.keys().collect();
    marker_indices.sort();
    for idx in marker_indices {
        println!("  marker before {:?}: {}", items[*idx].path.file_name().unwrap_or_default(), markers[idx]);
    }
    for (n, batch) in batches.iter().enumerate() {
        let bytes: u64 = batch.iter().map(|&i| items[i].size_bytes).sum();
        println!("  batch {n}: {} clip(s), {:.2} GiB", batch.len(), bytes as f64 / ONE_GIB as f64);
    }

    if args.dry_run {
        println!("Dry run: no video was produced.");
        return Ok(());
    }

    ensure_font_available()?;

    fs::create_dir_all(&args.work_dir)?;
    if let Some(parent) = args.output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    println!("Building {} batch(es) with up to {} in parallel...", batches.len(), args.jobs);
    let batch_outputs = build_batches_parallel(
        &args.work_dir,
        &items,
        &batches,
        &markers,
        &rotations,
        args.photo_seconds,
        !args.no_overlays,
        args.jobs,
    )?;

    let outro = generate_outro(&args.work_dir)?;
    let mut concat_inputs = batch_outputs;
    concat_inputs.push(outro);

    println!("Concatenating batches...");
    let concat_path = concat_batches(&args.work_dir, &concat_inputs)?;

    println!("Adding title and background song...");
    build_final(&args.work_dir, &concat_path, &args.song, &args.title, args.song_volume, &args.output)?;

    println!("Done: {:?}", args.output);
    println!("Intermediate files kept at {:?}", args.work_dir);
    Ok(())
}
