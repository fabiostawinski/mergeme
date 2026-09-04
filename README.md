mergeme - merge exported Google Photos videos and pictures into one YouTube-ready video

## Install

Prebuilt releases are published on the [GitHub Releases page](https://github.com/fabiostawinski/mergeme/releases).
Download the archive for your platform, or use the installer below.

macOS/Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/fabiostawinski/mergeme/main/scripts/install-mergeme.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/fabiostawinski/mergeme/main/scripts/install-mergeme.ps1 | iex
```

The installers support `MERGEME_VERSION` for a release such as `mergeme-v0.1.0`
(`0.1.0`, `v0.1.0`, and the full tag are accepted) and
`MERGEME_PREFIX` for a custom install directory. The shell installer also
supports Apple Silicon and Intel macOS plus x86_64 Linux. The Windows release
currently targets x86_64.

To publish a release, create and push a tag in this format:

```sh
git tag mergeme-v0.1.0
git push origin mergeme-v0.1.0
```

## Runtime requirements

`mergeme` requires `ffmpeg`, `ffprobe`, and `exiftool` on `PATH`. Check the
installation before running it:

```sh
./scripts/check-tools.sh
```

On Windows, run `scripts\\check-tools.ps1`. Typical package-manager commands:

```sh
# macOS
brew install ffmpeg exiftool

# Debian or Ubuntu
sudo apt-get install ffmpeg libimage-exiftool-perl
```

```powershell
# Windows (WinGet)
winget install Gyan.FFmpeg OliverBetz.ExifTool
```

## Support the project

If `mergeme` is useful to you, you can support development through
[GitHub Sponsors](https://github.com/sponsors/fabiostawinski). GitHub Sponsors
is the primary donation platform because it is linked directly to the project
and supports recurring or one-time contributions. Ko-fi and Liberapay are
reasonable alternatives if a separate donation page is preferred.

Point it at a folder of exported videos and pictures (e.g. from Google
Takeout) and it produces a single video: sorted chronologically, with a
5-second "Month Year" title card inserted wherever the timeline crosses into
a new month, your title burned onto the first second, and a background song
mixed in quietly under the original audio, looping for as long as the video
runs. Pictures are shown as a still clip (2 seconds by default, see
`--photo-seconds`) with silent audio, interleaved chronologically with the
videos. The video ends on a 3-second black screen, with the song fading out
under it.

Videos and pictures are grouped into ~1GB batches (a single file bigger than
that gets its own batch) and each batch is encoded in parallel, then joined
together.

## Usage

- Preview the plan (no video produced):

  cargo run --bin mergeme -- --input ./exported --output ./out/trip.mp4 \
    --title "Our Trip" --song ./song.mp3 --dry-run

- Actually build it:

  cargo run --bin mergeme -- --input ./exported --output ./out/trip.mp4 \
    --title "Our Trip" --song ./song.mp3

Flags:
- `--input <dir>` (required) — folder to scan recursively for videos (`.mp4`, `.mov`, `.m4v`, `.avi`, `.mkv`) and pictures (`.jpg`, `.jpeg`, `.png`, `.heic`, `.heif`, `.webp`)
- `--output <file>` (required) — final video path
- `--title <text>` (required) — burned onto the first second of the video
- `--song <file>` (required) — background audio; looped or trimmed to fit, mixed at reduced volume
- `--song-volume <0.0-1.0>` — default `0.15`
- `--work-dir <dir>` — where intermediate files go; default `.<output-stem>-work` next to the output. Not cleaned up automatically.
- `--jobs <n>` — how many batches to encode in parallel; default up to 4
- `--photo-seconds <n>` — how long each picture is shown for; default `2`
- `--no-overlays` — skip the "Month Year" title cards and the burned-in date/time label on each clip
- `--marker <filename-or-path>=<label text>` — insert a bumper card with custom text right before that clip; works even with `--no-overlays`, and repeatable for multiple markers
- `--marker <label text>` (no `=`) — insert a bumper card with that text at the very start of the video, overriding whatever else would land there
- `--dry-run` — print the plan (video/picture count, markers, batches) without encoding

## Capture date

Each video's or picture's capture time is resolved, in order: a Google
Takeout sidecar JSON's `photoTakenTime.timestamp` (most reliable when
exported via Takeout), then EXIF `CreateDate`/`DateTimeOriginal` via
`exiftool`, then filesystem modified time.

## Notes

- Requires `ffmpeg`, `ffprobe`, and `exiftool` on `PATH`.
- All videos are re-encoded to a common 1920x1080/30fps/AAC-stereo format so
  heterogeneous phone footage (varying resolution, fps, mono/stereo audio)
  concatenates cleanly.
- Same-named collisions aren't a concern here since every run produces one
  fresh output file rather than moving files into a shared tree (unlike
  `io2`).
