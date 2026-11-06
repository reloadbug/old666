//! 构建脚本。
//!
//! 除了 Tauri 的默认检查，这里额外盯住**前端每个文件的修改时间**。
//!
//! 原因：`tauri_build::build()` 只对 `frontendDist` 目录做 `rerun-if-changed`，
//! 而 Cargo 对目录的追踪只看「目录本身有没有变」（增删文件），**不跟踪目录里
//! 已有文件的内容修改**。于是 `src/index.html` / `boot.js` 改完再
//! `cargo build --release`，构建脚本不会重跑，嵌进 exe 的还是旧前端 ——
//! 表现就是「代码明明改了，界面上却没变」，极难排查（2026-10-20 踩过）。
//!
//! 逐个文件登记后，任何一处前端改动都会触发重新嵌入。

use std::path::Path;

fn main() {
    // 前端目录：src-tauri/../src
    let dist = Path::new("../src");
    if dist.is_dir() {
        watch_dir(dist);
    }
    tauri_build::build()
}

/// 递归登记目录下所有文件的修改时间。
fn watch_dir(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            watch_dir(&p);
        } else {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
}
