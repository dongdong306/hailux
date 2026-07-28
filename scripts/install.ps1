$ErrorActionPreference = "Stop"

$Repo = "dongdong306/hailux"
$InstallDir = "$env:LOCALAPPDATA\hailux"
$BinaryName = "hailux.exe"

Write-Host "==> Downloading hailux..." -ForegroundColor Blue

$DownloadUrl = "https://github.com/$Repo/releases/latest/download/hailux-windows-amd64.exe"

# Create install directory
if (!(Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

$DestPath = Join-Path $InstallDir $BinaryName

try {
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $DestPath -UseBasicParsing
} catch {
    Write-Host "Error: Failed to download hailux." -ForegroundColor Red
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}

Write-Host "==> Installed to $DestPath" -ForegroundColor Green

# Add to user PATH if not already there
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($CurrentPath -notlike "*$InstallDir*") {
    $NewPath = if ($CurrentPath) { "$CurrentPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    Write-Host "==> Added $InstallDir to user PATH." -ForegroundColor Yellow
    Write-Host "    Restart your terminal for PATH changes to take effect." -ForegroundColor Yellow
} else {
    Write-Host "==> $InstallDir is already in PATH." -ForegroundColor DarkGray
}

Write-Host "==> hailux installed successfully!" -ForegroundColor Green
Write-Host ""
Write-Host "Run 'hailux' to get started." -ForegroundColor Cyan
