## Install

macOS/Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/fabiostawinski/mergeme/main/scripts/install-mergeme.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/fabiostawinski/mergeme/main/scripts/install-mergeme.ps1 | iex
```

Both installers fetch the latest release by default; set `MERGEME_VERSION` to pin
a specific one. Runtime tools required: `ffmpeg`, `ffprobe`, and `exiftool` — see
the [README](https://github.com/fabiostawinski/mergeme#runtime-requirements).
