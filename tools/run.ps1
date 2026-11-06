# 老六清净 old666 · 本地测试启动脚本
#
# 用途：改完代码直接跑起来看效果，不用打包、不用手动找 exe。
#       增量编译（只重编改动部分，前端改动会重新嵌入 exe），编完自动拉起窗口。
#
# 用法（在仓库根目录）：
#   run.cmd                     # 双击，dev-fast 增量编译 + 提权启动（日常就用这个）
#   run.cmd -NoBuild            # 跳过编译，直接启动上一次的 exe（最快，秒开）
#   run.cmd -NoElevate          # 不提权启动（不弹 UAC，开机加速页会少列一些）
#   run.cmd -ResetData          # 启动前清空测试数据（会二次确认）
#   run.cmd -DryRun             # 只编译 + 检查环境，不启动窗口
#   run.cmd -Release            # 用正式 release 档编译（体积优先，慢 3 倍）
#   run.cmd -Debug              # 用 debug 档编译（编译最快，启动最慢）
#
# 关于 profile：默认走 `dev-fast`（Cargo.toml 里自定义），关掉 LTO、
# 放开并行 codegen，前端改一行重编约 35 s；正式 release 档同样改动约 110 s。
# 两者产物分开存放，互不影响。

param(
    [switch]$Release,
    [switch]$Debug,
    [switch]$NoBuild,
    [switch]$NoElevate,
    [switch]$DryRun,
    [switch]$ResetData
)

$ErrorActionPreference = 'Stop'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

function Step($m) { Write-Host "  $m" }
function Ok($m) { Write-Host "  [OK] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "  [!] $m" -ForegroundColor Yellow }
function Die($m) { Write-Host ""; Write-Host "  [X] $m" -ForegroundColor Red; exit 1 }

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host ""
Write-Host "  old666 本地测试启动" -ForegroundColor Cyan
Write-Host "  -------------------"

# ---------------------------------------------------------------- 1. 工具链
# 与 tools/env.sh 保持同一套镜像配置，避免脚本和 bash 两套环境打架。
if (-not $env:CARGO_HOME) {
    if (Test-Path 'D:\DevTools\cargo') { $env:CARGO_HOME = 'D:\DevTools\cargo' }
}
if (-not $env:RUSTUP_HOME) {
    if (Test-Path 'D:\DevTools\rustup') { $env:RUSTUP_HOME = 'D:\DevTools\rustup' }
}

$cargo = $null
$tries = @()
if ($env:CARGO_HOME) { $tries += (Join-Path $env:CARGO_HOME 'bin\cargo.exe') }
$tries += 'D:\DevTools\cargo\bin\cargo.exe'
if ($env:USERPROFILE) { $tries += (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe') }
$onPath = Get-Command cargo -ErrorAction SilentlyContinue
if ($onPath) { $tries += $onPath.Source }
foreach ($t in $tries) {
    if ($t -and (Test-Path $t)) { $cargo = $t; break }
}

# ---------------------------------------------------------------- 2. 编译
if ($Release -and $Debug) { Die '-Release 和 -Debug 只能选一个。' }

if ($Release) {
    $profileDir = 'release'
    $buildArgs = @('--release')
    $label = 'release · 体积优先'
}
elseif ($Debug) {
    $profileDir = 'debug'
    $buildArgs = @()
    $label = 'debug · 编译最快'
}
else {
    $profileDir = 'dev-fast'
    $buildArgs = @('--profile', 'dev-fast')
    $label = 'dev-fast · 日常档'
}

$exe = Join-Path $root ("src-tauri\target\{0}\old666.exe" -f $profileDir)

if ($NoBuild) {
    if (-not (Test-Path $exe)) {
        Die ("还没有编译过：`n      {0}`n      去掉 -NoBuild 再跑一次（首次编译约 1 分钟）。" -f $exe)
    }
    Step "跳过编译（-NoBuild）"
}
else {
    if (-not $cargo) {
        Die "找不到 cargo。装好 Rust 工具链，或把 cargo 放进 PATH 后重试。"
    }
    Step "编译中（$label）… 前端改动会重新嵌入 exe；改前端约 35 s（release 档约 110 s）"
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Push-Location (Join-Path $root 'src-tauri')
    # cargo 把进度写到 stderr；PS 5.1 在 EAP=Stop 时会把 stderr 当 NativeCommandError 抛出，
    # 所以这一段必须临时降成 Continue，否则「编译成功」也会被判成失败。
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $cargo build @buildArgs --bin old666 2>&1 | ForEach-Object {
            $line = [string]$_
            if ($line -match '^\s*error') { Write-Host "    $line" -ForegroundColor Red }
            elseif ($line -match '^\s*(Compiling old666|Finished)') { Write-Host "    $line" -ForegroundColor DarkGray }
        }
        $code = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $prevEap
        Pop-Location
    }
    $sw.Stop()
    if ($code -ne 0) {
        $hint = ($buildArgs -join ' ')
        Die ("编译失败（exit {0}）。完整日志：`n      cd src-tauri; cargo build --bin old666 {1}" -f $code, $hint)
    }
    Ok ("编译完成 {0:N1} s" -f $sw.Elapsed.TotalSeconds)
}

if (-not (Test-Path $exe)) { Die "编译产物不存在：$exe" }

$exeDir = Split-Path -Parent $exe
$info = Get-Item $exe
$stamp = $info.LastWriteTime.ToString('MM-dd HH:mm:ss')
Step ("exe : {0}" -f $exe)
Step ("      {0:N2} MB · 构建于 {1}" -f ($info.Length / 1MB), $stamp)

# ---------------------------------------------------------------- 3. 数据目录
$dataDir = Join-Path $exeDir 'data'
Step ("数据 : {0}" -f $dataDir)

if ($ResetData) {
    if (Test-Path $dataDir) {
        $ans = Read-Host "  清空测试数据（白名单 / 变更批次 / 快照）？输入 y 确认"
        if ($ans -eq 'y' -or $ans -eq 'Y') {
            Remove-Item $dataDir -Recurse -Force
            Ok '已清空测试数据'
        }
        else { Step '已取消，保留原数据' }
    }
    else { Step '没有数据目录，无需清空' }
}

# ---------------------------------------------------------------- 4. 已有实例
$running = @(Get-Process -Name 'old666' -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    $pids = ($running | ForEach-Object { $_.Id }) -join ', '
    Warn ("已有 {0} 个 old666 实例在运行（PID {1}）" -f $running.Count, $pids)
    if ($DryRun) {
        Warn '两个实例会共用同一个数据目录，正式启动前建议先关掉旧的'
    }
    else {
        $ans = Read-Host "  先关闭它们再启动？（y/N）"
        if ($ans -eq 'y' -or $ans -eq 'Y') {
            # 只发关闭窗口消息，不强制结束进程 —— 强杀一律要单独确认。
            foreach ($p in $running) {
                try { $p.CloseMainWindow() | Out-Null } catch { }
            }
            Start-Sleep -Milliseconds 900
            $left = @(Get-Process -Name 'old666' -ErrorAction SilentlyContinue)
            if ($left.Count -gt 0) {
                Warn '仍有实例没关掉，请手动关闭后再运行（不替你强杀）'
                Die '已中止，未启动新实例'
            }
            Ok '旧实例已关闭'
        }
        else {
            Warn '继续启动：两个实例共用同一数据目录，可能互相覆盖批次记录'
        }
    }
}

# ---------------------------------------------------------------- 5. 启动
if ($DryRun) {
    Ok '检查通过（-DryRun，不启动窗口）'
    exit 0
}

$isAdmin = $false
try {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $isAdmin = ([Security.Principal.WindowsPrincipal]$id).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}
catch { }

if ($isAdmin) {
    Step '当前已是管理员，直接启动'
    Start-Process -FilePath $exe -WorkingDirectory $exeDir | Out-Null
    Ok '已启动'
}
elseif ($NoElevate) {
    Step '按 -NoElevate 以普通权限启动'
    Start-Process -FilePath $exe -WorkingDirectory $exeDir | Out-Null
    Ok '已启动（开机加速页会少列一些受保护项）'
}
else {
    Step '请求管理员权限 —— UAC 弹窗请点「是」'
    try {
        Start-Process -FilePath $exe -WorkingDirectory $exeDir -Verb RunAs | Out-Null
        Ok '已启动（管理员）'
    }
    catch {
        Warn '提权被取消，改用普通权限启动'
        Start-Process -FilePath $exe -WorkingDirectory $exeDir | Out-Null
        Ok '已启动（普通权限）'
    }
}
