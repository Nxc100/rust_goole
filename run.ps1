# 运行脚本：转发参数给翻译程序（优先用 release，其次 debug）。
# 用法示例：
#   ./run.ps1 -i "D:\图片\input" -o "D:\图片\out" -t en
#   ./run.ps1 -i "D:\图片\input" -o "D:\图片\out_ja" -t ja -s zh-CN
param([Parameter(ValueFromRemainingArguments = $true)] $RestArgs)
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$exe = Join-Path $PSScriptRoot "target\release\google_img_translate.exe"
if (-not (Test-Path $exe)) {
    $exe = Join-Path $PSScriptRoot "target\debug\google_img_translate.exe"
}
if (-not (Test-Path $exe)) {
    Write-Error "未找到可执行文件，请先运行 ./build.ps1 构建。"
    exit 1
}
& $exe @RestArgs
exit $LASTEXITCODE
