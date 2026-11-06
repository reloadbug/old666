#!/usr/bin/env bash
# 导出「开机加速」前端测试所需的真机 fixture（全部是只读命令，零副作用）。
#
#   bash tests/gen-fixtures.sh          # 默认用 debug 构建的 CLI
#   POC=/path/to/old666-poc bash tests/gen-fixtures.sh
#
# 产出（tests/_out/fixtures/，已 gitignore）：
#   boot.json   --boot --json             8 次开机的分段耗时与拖慢归因
#   fast.json   --startup --fast --json   未补签的首屏扫描
#   deep.json   --startup --json          补签后的完整扫描
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
POC="${POC:-$ROOT/src-tauri/target/debug/old666-poc.exe}"
OUT="$ROOT/tests/_out/fixtures"

if [[ ! -x "$POC" ]]; then
  echo "找不到 $POC —— 先跑：cd src-tauri && cargo build --bin old666-poc" >&2
  exit 1
fi

mkdir -p "$OUT"
echo "导出 fixture 到 $OUT"
"$POC" --boot            --json > "$OUT/boot.json"
"$POC" --startup --fast   --json > "$OUT/fast.json"
"$POC" --startup          --json > "$OUT/deep.json"
for f in boot.json fast.json deep.json; do
  printf '  %-12s %6.1f KB\n' "$f" "$(wc -c < "$OUT/$f" | awk '{print $1/1024}')"
done
echo "完成。接着跑：node tests/frontend_boot.mjs"
