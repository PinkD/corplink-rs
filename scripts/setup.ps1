# Download the pinned amd64 Wintun DLL; this script does not load the driver.
# Usage: powershell -File setup.ps1
$ErrorActionPreference = "Stop"

$WintunVersion = "0.14.1"
$WintunUrl = "https://www.wintun.net/builds/wintun-$WintunVersion.zip"
$ZipSha256 = "07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51"
$DllSha256 = "e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce"
$ScriptDir = $PSScriptRoot
$Destination = Join-Path $ScriptDir "wintun.dll"

function Assert-Hash([string]$Path, [string]$Expected) {
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $Expected) {
        throw "SHA-256 mismatch for $Path. Refusing to use this file."
    }
}

if (Test-Path -LiteralPath $Destination) {
    Assert-Hash $Destination $DllSha256
    Write-Host "Verified existing Wintun $WintunVersion (amd64)."
    exit 0
}

# Use a fresh directory instead of reusing shared predictable ZIP/extract paths.
$WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ("corplink-wintun-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $WorkDir -ErrorAction Stop | Out-Null
try {
    $ZipFile = Join-Path $WorkDir "wintun.zip"
    Invoke-WebRequest -Uri $WintunUrl -OutFile $ZipFile -UseBasicParsing
    Assert-Hash $ZipFile $ZipSha256
    Expand-Archive -LiteralPath $ZipFile -DestinationPath $WorkDir
    $DllPath = Join-Path $WorkDir "wintun/bin/amd64/wintun.dll"
    Assert-Hash $DllPath $DllSha256
    # Copy without overwrite: do not silently replace a file that appeared meanwhile.
    [System.IO.File]::Copy($DllPath, $Destination, $false)
    Assert-Hash $Destination $DllSha256
    Write-Host "Verified Wintun $WintunVersion (amd64) is ready."
} finally {
    Remove-Item -LiteralPath $WorkDir -Recurse -Force
}
