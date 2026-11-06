#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P0 性能基线测量 —— 对照 docs/实施计划.md 的 P0 验收标准。

测量项：
  1. 便携包体积       目标 ≤ 12 MB
  2. 冷启动到可交互   目标 ≤ 1.0 s
  3. 采集 1000 进程   目标 ≤ 200 ms（本机进程数会如实标注）
  4. 进程枚举 / 结束进程 / 工作集裁剪 链路可用性

用法：
    python tools/measure-p0.py            # 人类可读
    python tools/measure-p0.py --json     # 输出 JSON（供报告脚本消费）
"""

import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "src-tauri" / "target" / "release" / "old666.exe"
POC = ROOT / "src-tauri" / "target" / "release" / "old666-poc.exe"

# P0 验收阈值
BUDGET_PACKAGE_MB = 12.0
BUDGET_COLD_START_MS = 1000.0
BUDGET_SCAN_MS = 200.0
COLD_START_RUNS = 7


def human(n: float, unit: str = "B") -> str:
    for u in ["B", "KB", "MB", "GB"]:
        if n < 1024 or u == "GB":
            return f"{n:.1f} {u}" if u != "B" else f"{int(n)} B"
        n /= 1024
    return f"{n:.1f} GB"


def dir_size(p: Path) -> int:
    return sum(f.stat().st_size for f in p.rglob("*") if f.is_file())


def measure_cold_start() -> dict:
    """反复启动 --bench-startup，取中位数与最大值。"""
    if not EXE.exists():
        return {"ok": False, "error": f"未找到 {EXE}"}

    in_app, wall = [], []
    for _ in range(COLD_START_RUNS):
        t0 = time.perf_counter()
        try:
            r = subprocess.run(
                [str(EXE), "--bench-startup"],
                capture_output=True, text=True, timeout=30,
            )
        except subprocess.TimeoutExpired:
            return {"ok": False, "error": "启动超时"}
        w = (time.perf_counter() - t0) * 1000.0
        wall.append(w)
        out = (r.stdout or "").strip().splitlines()
        try:
            in_app.append(float(out[-1]))
        except (ValueError, IndexError):
            pass

    return {
        "ok": True,
        "runs": COLD_START_RUNS,
        "in_app_median_ms": round(statistics.median(in_app), 1) if in_app else None,
        "in_app_max_ms": round(max(in_app), 1) if in_app else None,
        "wall_median_ms": round(statistics.median(wall), 1),
        "wall_min_ms": round(min(wall), 1),
    }


def measure_poc() -> dict:
    if not POC.exists():
        return {"ok": False, "error": f"未找到 {POC}"}
    r = subprocess.run([str(POC), "--json", "--quiet"], capture_output=True, text=True, timeout=120)
    if r.returncode != 0:
        return {"ok": False, "error": (r.stderr or "")[-600:]}
    try:
        return {"ok": True, "data": json.loads(r.stdout)}
    except json.JSONDecodeError as e:
        return {"ok": False, "error": f"JSON 解析失败：{e}", "raw": r.stdout[:400]}


def main() -> int:
    as_json = "--json" in sys.argv
    rep = {"measured_at": time.strftime("%Y-%m-%d %H:%M:%S")}

    # 1. 体积
    exe_bytes = EXE.stat().st_size if EXE.exists() else 0
    rep["package"] = {
        "exe_bytes": exe_bytes,
        "exe_mb": round(exe_bytes / 1048576, 2),
        "budget_mb": BUDGET_PACKAGE_MB,
        "pass": exe_bytes > 0 and exe_bytes / 1048576 <= BUDGET_PACKAGE_MB,
    }

    # 2. 冷启动
    rep["cold_start"] = measure_cold_start()
    cs = rep["cold_start"]
    cs["budget_ms"] = BUDGET_COLD_START_MS
    cs["pass"] = bool(cs.get("ok") and cs.get("in_app_median_ms")
                      and cs["in_app_median_ms"] <= BUDGET_COLD_START_MS)

    # 3. PoC 采集
    rep["poc"] = measure_poc()
    if rep["poc"].get("ok"):
        d = rep["poc"]["data"]
        rep["poc"]["pass_scan"] = d["scan"]["total_ms"] <= BUDGET_SCAN_MS
        rep["poc"]["budget_scan_ms"] = BUDGET_SCAN_MS

    if as_json:
        print(json.dumps(rep, ensure_ascii=False, indent=2))
        return 0

    # ---- 人类可读 ----
    line = "─" * 66
    print(f"\n{line}\n  老六清净 old666 · P0 验收基线\n{line}")

    p = rep["package"]
    print(f"\n【包体积】")
    print(f"  old666.exe         : {p['exe_mb']:.2f} MB")
    print(f"  验收 (≤{BUDGET_PACKAGE_MB:.0f} MB)    : {'通过 ✓' if p['pass'] else '未达标 ✗'}")

    print(f"\n【冷启动到可交互】")
    if cs.get("ok"):
        print(f"  应用内计时中位     : {cs['in_app_median_ms']} ms")
        print(f"  应用内计时最大     : {cs['in_app_max_ms']} ms")
        print(f"  进程端到端中位     : {cs['wall_median_ms']} ms（含 exe 加载 + 退出）")
        print(f"  验收 (≤{BUDGET_COLD_START_MS:.0f} ms)  : {'通过 ✓' if cs['pass'] else '未达标 ✗'}")
    else:
        print(f"  失败               : {cs.get('error')}")

    print(f"\n【进程采集】")
    poc = rep["poc"]
    if poc.get("ok"):
        d = poc["data"]
        print(f"  进程数             : {d['enumerate']['process_count']}")
        print(f"  枚举中位 / 最大    : {d['enumerate']['median_ms']} / {d['enumerate']['max_ms']} ms")
        print(f"  完整扫描（含路径） : {d['scan']['total_ms']} ms")
        print(f"  验收 (≤{BUDGET_SCAN_MS:.0f} ms)   : {'通过 ✓' if poc['pass_scan'] else '未达标 ✗'}")
        print(f"  判定分布           : 可结束 {d['verdict']['safe']} / 保留 {d['verdict']['keep']} / 禁止 {d['verdict']['forbidden']}")
        print(f"  PDH 可用           : {'是 ✓' if d['pdh']['healthy'] else '否 ✗'}")
        print(f"  签名抽样           : {d['signature']['signed']} 已签 / {d['signature']['unsigned']} 未签")
        kc = d["kill_chain"]
        print(f"  结束进程链路       : 拉起 {kc['spawned']} → 成功 {kc['killed']}（仍存活 {kc['still_alive']}）")
        ews = d["empty_working_set"]
        print(f"  工作集裁剪         : {'成功' if ews['ok'] else '失败'}，释放 {human(ews['freed_bytes'])}")
    else:
        print(f"  失败               : {poc.get('error')}")

    print(f"\n{line}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
