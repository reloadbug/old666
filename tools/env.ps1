# 在 PowerShell 里加载本文件即可把 old666 的构建环境配好。
# 用法：  . .\tools\env.ps1
#
# 约定（重要）：
# * 所有构建产物一律落在**项目内**的 .work\target，方便统一清理；
#   历史上曾用项目外的 D:\tmp_probe_target，会在 D 盘留下一堆无人认领的垃圾。
# * 不要用 Set-Content 改 .rs 文件 —— PS 5.1 的 utf8 编码会写 BOM 并破坏中文注释。

$env:CARGO_HOME = 'D:\Program Files\cargo'
$env:RUSTUP_HOME = 'D:\Program Files\rustup'
$env:CARGO_TARGET_DIR = 'D:\AI game\old666\.work\target'
$env:ErrorActionPreference = 'Continue'

New-Item -ItemType Directory -Force -Path $env:CARGO_TARGET_DIR | Out-Null

# 让 cargo 出现在 PATH 里
$env:PATH = "$env:CARGO_HOME\bin;$env:PATH"

# node（前端无头测试用）
$nodeBin = 'C:\Users\Administrator\.dsh\dsh-runtimes\dsh-primary-runtime\dependencies\node\bin'
if (Test-Path $nodeBin) { $env:PATH = "$nodeBin;$env:PATH" }

Write-Host "old666 构建环境已就绪："
Write-Host "  CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR"
Write-Host "  cargo            = $((Get-Command cargo.exe -ErrorAction SilentlyContinue).Source)"
Write-Host "  node             = $((Get-Command node.exe -ErrorAction SilentlyContinue).Source)"
