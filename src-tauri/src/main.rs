//! `old666` GUI 入口。
//!
//! release 构建下不弹控制台窗口（`windows_subsystem = "windows"`），
//! 但 `eprintln!` 的诊断信息仍会写进 stderr —— 便携版建议从终端启动一次
//! 以确认提权与 PDH 状态。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    old666_lib::run()
}
