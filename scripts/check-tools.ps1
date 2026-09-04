$ErrorActionPreference = 'Continue'
$missing = @()

foreach ($tool in @('ffmpeg', 'ffprobe', 'exiftool')) {
    $command = Get-Command $tool -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        Write-Host "OK      ${tool}: $($command.Source)"
    } else {
        Write-Host "MISSING ${tool}"
        $missing += $tool
    }
}

if ($missing.Count -gt 0) {
    Write-Host ""
    Write-Host "Install the missing runtime tools, then run this check again."
    Write-Host "Windows: winget install Gyan.FFmpeg OliverBetz.ExifTool"
    exit 1
}

Write-Host 'All mergeme runtime tools are available.'
