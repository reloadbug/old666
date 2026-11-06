#!/usr/bin/env bash
# 老六清净 old666 · 构建环境一键注入
#
# 本沙箱对 static.rust-lang.org / static.crates.io 限流严重（6 KB/s / 91 KB/s），
# 因此 Rust 工具链与 crates 源都指向 USTC 镜像（实测 2.3 MB/s / 1.5 MB/s）。
#
# 用法：  source tools/env.sh
#         cargo build --release

export RUSTUP_HOME='D:\DevTools\rustup'
export CARGO_HOME='D:\DevTools\cargo'
export RUSTUP_DIST_SERVER='https://mirrors.ustc.edu.cn/rust-static'
export RUSTUP_UPDATE_ROOT='https://mirrors.ustc.edu.cn/rust-static/rustup'
export PATH="$CARGO_HOME/bin:$PATH"

# MSVC 工具链（VS 2022 Build Tools 装在 D 盘）
export MSVC_DIR='D:\DevTools\VS2022BuildTools'
