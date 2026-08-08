# sView リリースビルドスクリプト (Windows PowerShell)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Rust が必要です: https://rustup.rs/"
}
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    Write-Error "Node.js が必要です: https://nodejs.org/"
}

npm install
npm run build

Write-Host ""
Write-Host "ビルド完了。成果物: src-tauri\target\release\bundle\"
