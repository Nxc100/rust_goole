# 构建脚本：自动把 MinGW-w64 加入 PATH，并用 GNU 工具链编译 release 版本。
# 用法：在本目录执行  ./build.ps1
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# MinGW-w64 bin 路径（如有变动，改这里或先自行加入 PATH）
$mingw = "D:\xc_test\new_proj\rust_prjo\toolchain\mingw64\bin"
if (Test-Path $mingw) {
    $env:PATH = "$mingw;$env:PATH"
} elseif (-not (Get-Command gcc.exe -ErrorAction SilentlyContinue)) {
    Write-Warning "未找到 MinGW（$mingw 不存在，且 PATH 中无 gcc）。GNU 工具链构建需要 MinGW 的 gcc/dlltool。"
}

Set-Location $PSScriptRoot
cargo build --release
if ($LASTEXITCODE -eq 0) {
    Write-Host "`n构建完成: $PSScriptRoot\target\release\google_img_translate.exe" -ForegroundColor Green
}
