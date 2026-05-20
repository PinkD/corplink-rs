# setup.ps1 — corplink-rs Windows 环境准备
# 自动下载 wintun.dll 到当前目录
# 用法: powershell -ExecutionPolicy Bypass -File setup.ps1

$ErrorActionPreference = "Stop"

$WintunVersion = "0.14.1"
$WintunUrl = "https://www.wintun.net/builds/wintun-$WintunVersion.zip"
$ZipFile = "wintun-$WintunVersion.zip"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

Write-Host "corplink-rs Windows setup" -ForegroundColor Cyan
Write-Host "========================" -ForegroundColor Cyan

# 检查 wintun.dll 是否已存在
if (Test-Path "$ScriptDir\wintun.dll") {
    $existing = Get-Item "$ScriptDir\wintun.dll"
    Write-Host "[OK] wintun.dll 已存在 ($($existing.VersionInfo.FileVersion))，跳过下载" -ForegroundColor Green
    Write-Host ""
    Write-Host "接下来:" -ForegroundColor Yellow
    Write-Host "  1. 编辑 config.json 填入你的公司代码和账号信息"
    Write-Host "  2. 以管理员身份运行: .\corplink-rs.exe config.json"
    exit 0
}

Write-Host "[1/3] 下载 wintun.dll (v$WintunVersion) ..." -ForegroundColor Yellow
Write-Host "        $WintunUrl"

try {
    Invoke-WebRequest -Uri $WintunUrl -OutFile "$env:TEMP\$ZipFile" -UseBasicParsing
} catch {
    Write-Host "[ERROR] 下载失败: $_" -ForegroundColor Red
    Write-Host ""
    Write-Host "请手动下载:" -ForegroundColor Yellow
    Write-Host "  1. 打开 https://www.wintun.net/"
    Write-Host "  2. 下载 wintun-$WintunVersion.zip"
    Write-Host "  3. 解压后找到 bin/amd64/wintun.dll"
    Write-Host "  4. 将 wintun.dll 复制到本目录"
    exit 1
}

Write-Host "[2/3] 解压 ..." -ForegroundColor Yellow
try {
    Expand-Archive -Path "$env:TEMP\$ZipFile" -DestinationPath "$env:TEMP\wintun-extract" -Force
} catch {
    Write-Host "[ERROR] 解压失败。请手动解压 $env:TEMP\$ZipFile" -ForegroundColor Red
    exit 1
}

Write-Host "[3/3] 复制 wintun.dll (amd64) 到当前目录 ..." -ForegroundColor Yellow
$DllPath = "$env:TEMP\wintun-extract\wintun-$WintunVersion\bin\amd64\wintun.dll"
if (-not (Test-Path $DllPath)) {
    Write-Host "[ERROR] 未找到 amd64/wintun.dll，请检查 wintun 包结构是否变化" -ForegroundColor Red
    Write-Host "        预期路径: $DllPath" -ForegroundColor Red
    exit 1
}

Copy-Item -Path $DllPath -Destination "$ScriptDir\wintun.dll"
Remove-Item -Path "$env:TEMP\$ZipFile" -ErrorAction SilentlyContinue
Remove-Item -Path "$env:TEMP\wintun-extract" -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "[DONE] wintun.dll 就绪" -ForegroundColor Green
Write-Host ""
Write-Host "接下来:" -ForegroundColor Yellow
Write-Host "  1. 编辑 config.json 填入你的公司代码和账号信息"
Write-Host "  2. 以管理员身份运行: .\corplink-rs.exe config.json"
Write-Host ""
Write-Host "高级用法:" -ForegroundColor Yellow
Write-Host "  调试日志: `$env:RUST_LOG='debug'; .\corplink-rs.exe config.json"
Write-Host "  指定服务端: 在 config.json 中设置 'vpn_server_name' 字段"
