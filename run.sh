#!/usr/bin/env bash
# old666 本地测试启动（Git Bash 入口，逻辑在 tools/run.ps1）
#   ./run.sh              增量编译 + 提权启动（日常）
#   ./run.sh -NoBuild     直接启动现有 exe
#   ./run.sh -DryRun      只编译检查不启动
#   ./run.sh -Release     用正式 release 档编译
set -euo pipefail
cd "$(dirname "$0")"
exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "tools/run.ps1" "$@"
