$ErrorActionPreference = 'Stop'

$repo = if ($env:MERGEME_REPO) { $env:MERGEME_REPO } else { 'fabiostawinski/mergeme' }
$version = if ($env:MERGEME_VERSION) { $env:MERGEME_VERSION } else { 'latest' }
$prefix = if ($env:MERGEME_PREFIX) { $env:MERGEME_PREFIX } else { Join-Path $HOME 'bin' }
$target = 'x86_64-pc-windows-msvc'
$url = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download/mergeme-$target.zip"
} else {
    $tag = if ($version -like 'mergeme-v*') {
        $version
    } elseif ($version -like 'v*') {
        "mergeme-$version"
    } else {
        "mergeme-v$version"
    }
    "https://github.com/$repo/releases/download/$tag/mergeme-$target.zip"
}

$temp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    $archive = Join-Path $temp 'mergeme.zip'
    Invoke-WebRequest -Uri $url -OutFile $archive
    Expand-Archive -Path $archive -DestinationPath $temp
    New-Item -ItemType Directory -Force -Path $prefix | Out-Null
    Copy-Item (Join-Path $temp "mergeme-$target\mergeme.exe") (Join-Path $prefix 'mergeme.exe') -Force
    Write-Host "Installed mergeme to $(Join-Path $prefix 'mergeme.exe')"

    $check = Get-Command ffmpeg, ffprobe, exiftool -ErrorAction SilentlyContinue
    if ($check.Count -lt 3) {
        Write-Host 'Runtime tools are still missing. Run scripts\check-tools.ps1'
    } else {
        Write-Host 'Runtime tools detected.'
    }
} finally {
    Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
}
