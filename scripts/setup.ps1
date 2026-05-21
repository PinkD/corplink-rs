# setup.ps1 — corplink-rs Windows environment setup
# Automatically downloads wintun.dll to the current directory
# Usage: powershell -ExecutionPolicy Bypass -File setup.ps1

$ErrorActionPreference = "Stop"

$WintunVersion = "0.14.1"
$WintunUrl = "https://www.wintun.net/builds/wintun-$WintunVersion.zip"
$ZipFile = "wintun-$WintunVersion.zip"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

Write-Host "corplink-rs Windows setup" -ForegroundColor Cyan
Write-Host "========================" -ForegroundColor Cyan

if (Test-Path "$ScriptDir\wintun.dll") {
    $existing = Get-Item "$ScriptDir\wintun.dll"
    Write-Host "[OK] wintun.dll found ($($existing.VersionInfo.FileVersion)), skip download" -ForegroundColor Green
    Write-Host ""
    Write-Host "Next steps:" -ForegroundColor Yellow
    Write-Host "  1. Edit config.json with your company code and account info"
    Write-Host "  2. Run as Administrator: .\corplink-rs.exe config.json"
    exit 0
}

Write-Host "[1/3] Downloading wintun.dll (v$WintunVersion) ..." -ForegroundColor Yellow
Write-Host "        $WintunUrl"

try {
    Invoke-WebRequest -Uri $WintunUrl -OutFile "$env:TEMP\$ZipFile" -UseBasicParsing
} catch {
    Write-Host "[ERROR] Download failed: $_" -ForegroundColor Red
    Write-Host ""
    Write-Host "Manual download:" -ForegroundColor Yellow
    Write-Host "  1. Visit https://www.wintun.net/"
    Write-Host "  2. Download wintun-$WintunVersion.zip"
    Write-Host "  3. Extract bin/amd64/wintun.dll"
    Write-Host "  4. Copy wintun.dll to this directory"
    exit 1
}

Write-Host "[2/3] Extracting ..." -ForegroundColor Yellow
try {
    Expand-Archive -Path "$env:TEMP\$ZipFile" -DestinationPath "$env:TEMP\wintun-extract" -Force
} catch {
    Write-Host "[ERROR] Extract failed. Manually extract $env:TEMP\$ZipFile" -ForegroundColor Red
    exit 1
}

Write-Host "[3/3] Copying wintun.dll (amd64) ..." -ForegroundColor Yellow
$DllPath = "$env:TEMP\wintun-extract\wintun-$WintunVersion\bin\amd64\wintun.dll"
if (-not (Test-Path $DllPath)) {
    Write-Host "[ERROR] amd64/wintun.dll not found. The wintun package structure may have changed." -ForegroundColor Red
    Write-Host "        Expected: $DllPath" -ForegroundColor Red
    exit 1
}

Copy-Item -Path $DllPath -Destination "$ScriptDir\wintun.dll"
Remove-Item -Path "$env:TEMP\$ZipFile" -ErrorAction SilentlyContinue
Remove-Item -Path "$env:TEMP\wintun-extract" -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "[DONE] wintun.dll is ready" -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Yellow
Write-Host "  1. Edit config.json with your company code and account info"
Write-Host "  2. Run as Administrator: .\corplink-rs.exe config.json"
Write-Host ""
Write-Host "Debug mode:" -ForegroundColor Yellow
Write-Host "  `$env:RUST_LOG='debug'; .\corplink-rs.exe config.json"
