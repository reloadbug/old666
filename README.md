# 老六清净 old666

> **老六最快** —— 一款无广告、纯绿色的 Windows 系统优化工具。
> 开机加速 · 运行加速 · 游戏加速。

不弹广告、不做全家桶、不偷跑后台。只解决用户真正在意的三件事：
**开机更快**、**久用不卡**、**一键净化不常用进程**。

---

## 核心特性

| 特性 | 说明 |
|---|---|
| 🚫 零广告 | 无推广位、无弹窗、无推送 |
| 🪶 极轻量 | 包体 ≤ 12 MB，常驻内存 ≤ 40 MB，冷启动 ≤ 1 s |
| 📦 绿色便携 | 解压即用，删除无残留，不写注册表 |
| 🔒 纯本地 | 默认不联网，不上传任何用户数据 |
| 🎯 一看就懂 | 一屏一焦点，主要优化收敛为一个按钮 |
| ↩️ 可回滚 | 每步操作留快照，一键恢复 |

---

## 功能模块

| 模块 | 能力 |
|---|---|
| **M1 开机加速** | 开机耗时时间线归因（误差 0%）、启动项/服务/计划任务统一管理、173 条进程知识库（用户可编辑）、风险分级、一键选中推荐项、白/黑名单（黑名单免确认直接禁用）、禁用与延迟启动、禁用项保留在列表并可单条启用、注册表项删除（自动备份） |
| **M2 进程净化** | 实时资源视图、不常用进程识别、智能分组、一键净化、白名单守护、一键回滚 |
| **M3 场景模式** | 日常 / 游戏 / 服务器三套规则方案，规则驱动、可解释、可自定义 |
| **M4 一键体检** | 30 秒扫描 → 健康评分 → 一键修复 → 前后对比 |
| **M5 内存与垃圾清理** | 工作集裁剪、临时文件/缓存/日志/回收站清理 |
| **M6 实时监控** | 桌面悬浮球，CPU / 内存 / 温度 / 网速，零打扰告警 |

---

## 快速开始

```bash
# 0. 注入构建环境（本机 Rust 装在 D 盘，crates 走 USTC 镜像）
source tools/env.sh

# 1. 跑单元测试（含 P1「零误杀」安全测试；测试默认处于演练模式，不动系统）
cd src-tauri && cargo test --lib

# 2. P0 性能基线 —— 默认只读，不会结束任何进程
cargo run --release --bin old666-poc
cargo run --release --bin old666-poc -- --list-safe   # 打印引擎实际判定计划（不执行）
cargo run --release --bin old666-poc -- --stress 200  # 200 轮压力测试，统计误杀

# 3. P2 开机加速 —— 同样只读，不会改动任何启动项
cargo run --release --bin old666-poc -- --boot              # 开机时间线 + 拖慢归因
cargo run --release --bin old666-poc -- --startup           # 启动项清单（含补签）
cargo run --release --bin old666-poc -- --startup --fast    # 只看首屏那一版（不补签）

# 4. 启动 GUI（推荐用根目录的 run.cmd / run.sh，见下一节）
cargo run --release --bin old666
```

> ⚠️ **写操作默认关闭**。PoC 的结束链路 / 工作集裁剪必须显式加 `--allow-write` 才会真正执行，
> 且执行前会再次过安全闸。启动项的禁用/延迟/还原可以从 GUI 发起（先出示演练清单、用户确认后才执行），
> CLI 侧的等价入口是 `--startup-apply`（同样要求 `--allow-write`）。
> GUI 与 CLI 共用同一套判定，不存在两套逻辑。

前端页面的无头验证（用真机 fixture 断言渲染，不需要起 GUI）：

```bash
bash tests/gen-fixtures.sh        # 从 CLI 现场导出 fixture（只读）
node tests/frontend_boot.mjs      # 111 项断言 + 截图，产出在 tests/_out/
```

---

## 本地启动（日常改代码用这个，不用打包）

根目录两个入口，逻辑都在 `tools/run.ps1`：

| 命令 | 说明 |
|---|---|
| `run.cmd`（双击） | 增量编译 + 提权启动，**日常就用这个** |
| `./run.sh` | 同上，Git Bash 入口 |
| `run.cmd -NoBuild` | 跳过编译直接启动上一次的 exe，秒开 |
| `run.cmd -DryRun` | 只编译 + 打印 exe/数据目录，不弹窗 |
| `run.cmd -NoElevate` | 不提权启动（不弹 UAC，开机加速页会少列受保护项） |
| `run.cmd -ResetData` | 启动前清空测试数据（会二次确认） |
| `run.cmd -Release` / `-Debug` | 换 profile 编译 |

**为什么不再需要打包**：脚本直接跑 `target/<profile>/old666.exe`，前端改动会由
`build.rs` 重新嵌入，编完自动拉起窗口。数据落在 exe 同级的 `data/`（默认
`src-tauri/target/dev-fast/data`），所以测试数据和源码互不干扰，`-ResetData` 一键重来。

**profile 选择**（`Cargo.toml` 里的 `dev-fast` 是自定义档）：

| profile | 改一行前端重编 | exe 体积 | 适用 |
|---|---|---|---|
| `dev-fast`（默认） | **约 35 s** | 4.6 MB | 日常迭代 |
| `release` | 约 110 s | 2.95 MB | 打包前最终验证 |
| `debug` | 最快（首次要编全部依赖） | 大 | 只调 Rust 逻辑 |

> `release` 为了 2.9 MB 的体积开了 `lto + codegen-units=1 + opt-level=z`，
> 代价是前端改一行也要重编整个 crate。`dev-fast` 关掉 LTO、放开并行 codegen，
> 只留 `opt-level=1`，启动速度与正式包基本一致 —— 体积大 1.6 MB 无所谓，反正不发布。

**踩过的坑**：`src-tauri/capabilities/` 目录原本不存在，但 `tauri-build` 会为它输出
`cargo:rerun-if-changed`，Cargo 把「监听路径缺失」当成永远脏 → **每次构建都全量重编**
（实测无改动也要 74 s）。补上标准的 `capabilities/default.json` 后，无改动构建降到 **2 s**。
构建变慢又找不到原因时，先跑 `cargo build -v 2>&1 | grep Dirty` 看它抱怨哪个文件。

---

打包便携包（v0.1.4）：

```bash
cargo build --release             # 两个二进制：old666.exe + old666-poc.exe
python tools/measure-p0.py        # P0 验收基线：体积 / 冷启动 / 采集耗时
# 打包（PowerShell）：
#   Compress-Archive -Path release\old666-v0.1.4-win64 -DestinationPath release\old666-v0.1.4-win64.zip
```

> 产物目录 `release/` 已加入 `.gitignore`：zip 是二进制，不进版本库；
> 包内容 = 两个 exe + 使用说明.txt + README + docs/，zip 解压即用、无需安装。

启动项写链路（禁用 / 延迟启动 / 还原）的真机实测（手动、可选）：

```powershell
powershell -ExecutionPolicy Bypass -File tests\service_write_verify.ps1
```

> 脚本自建一个临时服务做靶子、测完即删，全程不碰其他任何服务。
> 实测结果：延迟启动 3/3 生效并 3/3 复原；禁用→还原 1/1（Start 2→4→2）。

打包与图标：

```bash
node tools/build-icons.js          # 从品牌 SVG 生成 icons/（32/128/256 + ICO）
python tools/measure-p0.py         # 包体积 + 冷启动 + 采集耗时 对照 P0 验收
```

> **构建环境说明**：本沙箱对 `static.rust-lang.org` / `static.crates.io` 限流严重
> （实测 6 KB/s / 91 KB/s），因此工具链与 crates 源都指向 USTC 镜像
> （实测 2.3 MB/s / 1.5 MB/s）。`tools/env.sh` 已封装全部环境变量。

---

## 技术栈

- **前端**：HTML / CSS / TypeScript（Tauri WebView2）
- **后端**：Rust
- **系统调用**：`windows-rs`（Win32 / PDH / 注册表 / SCM / 任务计划）
- **打包**：Tauri 2，产出绿色便携版

> 五层架构：UI 表现层 → 桥接层（Tauri IPC）→ 核心服务层（Rust 模块）→ 系统抽象层（windows-rs）→ 操作系统层

---

## 文档

| 文档 | 内容 |
|---|---|
| [`docs/产品设计方案.html`](docs/产品设计方案.html) | 产品定位、功能规划、场景模式、UI/交互设计（含界面原型）、技术架构 |
| [`docs/实施计划.md`](docs/实施计划.md) | 分阶段实施计划 P0–P6、仓库结构、验收标准、安全红线 |
| [`docs/P0-P1-技术验证报告.md`](docs/P0-P1-技术验证报告.md) | 真机实测数据、10 条缺陷清单、判定引擎重建结论 |
| [`docs/P2-开机加速-技术说明.md`](docs/P2-开机加速-技术说明.md) | 开机事件日志真实 schema、两段式扫描、启动项四来源、无头测试抓出的 5 个 bug |
| [`assets/logo/showcase.html`](assets/logo/showcase.html) | 品牌标志规范：图形标、组合标、应用图标、使用禁区 |

## 品牌标志

图形标源自商标原始矢量文件（`assets/logo/source/reloadbug-flat.svg`，黑 + 赤红双色）。
原始文件实为「内嵌位图 + 分层引用」的伪 SVG，经**位图合成 → vtracer 矢量重绘 → 空路径清理 → 坐标烘焙**四步，
产出纯路径、单文档、可无限缩放的图形标：**925 KB → 8.8 KB，体积下降 105 倍**，形状完全保留。

| 文件 | 用途 | 体积 |
|---|---|---|
| `mark.svg` | 图形标 · 主用（浅色背景，墨黑 + 赤红双色） | 8.8 KB |
| `mark-dark.svg` | 图形标 · 深色背景版（白描边 + 赤红眼） | 8.8 KB |
| `mark-mono-dark.svg` / `mark-mono-light.svg` | 单色版（印章 / 水印 / 深底） | 8.8 KB |
| `mark-glossy.svg` | 光泽版 · 35 色高保真（大尺寸 / 启动页） | 95 KB |
| `lockup-horizontal.svg` / `lockup-vertical.svg` | 组合标（横版 / 竖版） | 9.5 KB |
| `app-icon.svg` / `app-icon-light.svg` | 应用图标（深色底 / 浅色底，1024²） | 9.3 KB |

配色：墨黑 `#0E1116`、赤红 `#F5340F`；`viewBox="285 326 931 847"`。

所有资源由 `tools/` 下的脚本自动生成，可随时重跑复现：

```bash
# 1. 位图合成 + 矢量重绘 → mark.svg（主标志）
python tools/vectorize-logo.py
# 2. 组合标 + 应用图标
python tools/build-brand-assets.py
# 3. 品牌规范页 showcase.html
python tools/build-brand-pages.py
# 4. 把图形标注入产品设计方案
python tools/inject-brand-into-doc.py
```

## 目录结构

```
old666/
├── docs/                    # 设计与计划文档
│   ├── 产品设计方案.html     # 产品/功能/UI/架构 设计方案
│   ├── 实施计划.md           # P0–P6 分阶段计划
│   └── P0-P1-技术验证报告.md # 实测数据、缺陷清单与结论
├── assets/logo/             # 品牌标志（source/ 为商标原始文件）
├── src/                     # 前端（Tauri WebView2，原生 HTML/CSS/JS，无框架）
│   ├── index.html
│   ├── styles.css
│   ├── main.js              # 进程净化页 + 视图切换
│   ├── boot.js              # 开机加速页（时间线 / 归因 / 启动项 / 禁用还原）
│   └── assets/              # 前端用的标志 SVG
├── tests/                   # 前端无头验证（真机 fixture + Playwright）
├── src-tauri/               # Rust 后端
│   ├── src/
│   │   ├── main.rs          # GUI 入口
│   │   ├── lib.rs           # 库入口 + Tauri 装配
│   │   ├── ipc.rs           # Tauri 命令层（不含业务判断）
│   │   ├── bin/poc.rs       # P0/P2 验证 CLI（默认只读）
│   │   └── core/            # 核心服务层（全部系统能力）
│   │       ├── safety.rs    # ★ 三层安全闸 + 演练熔断 + PID 白名单 + 启动项白名单
│   │       ├── blacklist.rs # 安全红线：237 进程名 / 47 服务 / 46 驱动
│   │       ├── process.rs   # NtQuerySystemInformation 枚举 + 采样器 + 窗口关系
│   │       ├── perf.rs      # PDH 性能计数器
│   │       ├── memory.rs    # 内存查询与裁剪
│   │       ├── signature.rs # WinVerifyTrust 签名校验
│   │       ├── idle.rs      # 六维闲置判定引擎 + 8 条封顶规则
│   │       ├── killer.rs    # 两段式结束 + EmptyWorkingSet（强制过安全闸）
│   │       ├── boot.rs      # 开机事件日志解析 + 分段归因 + 拖慢对象
│   │       ├── startup.rs   # 启动项枚举 + 两段式扫描（快速 / 补签）
│   │       ├── startup_risk.rs # 启动项风险分级规则
│   │       ├── startup_ctl.rs  # 启动项禁用 / 还原执行器（自包含撤销载荷）
│   │       ├── store.rs     # 白名单与快照（不写注册表）
│   │       ├── purge.rs     # 净化主流程编排（Live / DryRun 双模式）
│   │       └── types.rs     # 共享数据结构
│   ├── icons/               # 由 tools/build-icons.js 生成（不入库）
│   ├── tauri.conf.json
│   └── Cargo.toml
├── tools/                   # 品牌、图标、构建、测量脚本
│   ├── env.sh               # 构建环境注入（Rust 路径 + USTC 镜像）
│   ├── build-icons.js       # 品牌 SVG → 应用图标
│   ├── measure-p0.py        # 包体积 / 冷启动 / 采集耗时 对照验收
│   └── vectorize-logo.py 等 # 商标矢量化链路
├── .workbuddy-ai/skills/    # 项目技能（含「结束进程须人工确认」规则）
└── .github/workflows/       # CI（P5 后创建）
```

**分层约束**：`ipc.rs` 只做「加锁 → 调 core → 序列化」，**任何安全判断都不许写在这一层**。
否则 GUI 与 CLI 会出现两套判定逻辑，P1 的「零误杀」就无从保证。

---

## 开发状态

- ✅ 产品功能规划
- ✅ UI / 交互设计方案
- ✅ 技术架构设计
- ✅ 品牌标志（商标矢量化）
- ✅ 分阶段实施计划
- ✅ **P0 技术验证**（进程枚举 / PDH / 结束链路 / 工作集裁剪 / 性能基线）
- ✅ **P1 进程净化 MVP**（判定引擎 / 黑名单 / 签名 / 白名单 / 快照回滚 / UI）
- ✅ **P2 开机加速**（开机归因 / 启动项枚举 / 风险分级 / 禁用 / 延迟启动 / 前端无头验证）
- ✅ **第一版可运行版本 v0.1.3**（release 便携包，P0 验收基线全过，解压即用）
- ⬜ P3 场景模式引擎
- ⬜ P4 一键体检 + 垃圾清理
- ⬜ P5 监控悬浮球 + 打磨
- ⬜ P6 发布加固

**P0/P1 关键实测数据**（管理员权限，255 进程真机）：

| 指标 | 实测 | 验收 |
|---|---|---|
| 完整进程扫描 | 3.55 ms（中位 2.61 / 峰值 4.18） | ≤ 200 ms ✓ |
| 签名抽样 40 个 | 1.5 ms | — |
| 判定结果 | 可结束 1 / 保留 65 / 禁止 189 | 误杀 0 ✓ |
| 演练预估收益 | 结束 1 + 裁剪 56 ≈ **1.1 GB** | — |
| 200 轮压力测试 | 误杀 0，均值 3.1 ms / 峰值 5 ms | ✓ |
| 单元测试 | **131 passed / 0 failed** | ✓ |

**P2 关键实测数据**（真机，8 次开机的诊断日志 + 148 个启动项）：

| 指标 | 实测 | 说明 |
|---|---|---|
| 分段归因误差 | **0%**（验收要求 <10%） | 各阶段用 Windows 自报数字，残差单列一段，`Σ == BootTime` 恒成立 |
| 开机耗时 | 最快 31.8 s / 均值 61.0 s / 最慢 108.1 s | 最近一次 105.8 s（主路径 18.5 s + 桌面后台 87.3 s） |
| 拖慢归因 | `WorkBuddyAI.exe` 拖慢 54.1 s 等 4 个对象 | 按 `BootStartTime` 精确匹配到本次开机，不串场 |
| 启动项 | 148 项（注册表 16 / 文件夹 1 / 服务 105 / 计划任务 26） | 危险 101、可安全禁用候选 11 |
| 启动项快速扫描 | 154 ms 出列表，28 项待补签 | 首屏不阻塞 |
| 补签 | 745 ms（16 个文件合计 601 MB） | 串行 0.6 s vs 8 线程 2.1 s，**并行反而更慢** |
| 启动项归因回填 | 2.6 ms（latest 1.2 + 回填 1.4） | 让「拖慢开机 54.1 s」直接长在启动项上 |
| 延迟启动真机实测 | 设置 3/3 生效（注册表 `DelayedAutostart` 0→1），回滚 3/3 复原 | `ChangeServiceConfig2W` 走 SCM；`tests/service_write_verify.ps1` |
| 禁用→还原真机实测 | Start 2→4→2 完整复原 | 撤销载荷自包含，换机器也能还原 |
| 前端无头验证 | **111/111 断言通过** | `tests/frontend_boot.mjs`，真机 fixture |
| 服务可改性（真机） | 自动服务 95 个：**72 可改 / 23 受保护** | `old666-poc --svc-probe`（只读探测，不写任何值） |

**v0.1.1 / v0.1.2 修掉的三个「看起来没生效」的问题**（都是实测发现的真 bug，不是 UI 调整）：

| 问题 | 根因 | 修法 |
|---|---|---|
| 改了前端，release 包界面不更新 | `tauri_build` 只对 `frontendDist` **目录**做 `rerun-if-changed`，Cargo 不跟踪目录内已有文件的内容修改 → 构建脚本不重跑，嵌进 exe 的还是旧前端 | `build.rs` 递归登记 `src/` 下每个文件，任何改动都会触发重新嵌入 |
| 32 位启动项明明关了却显示「已启用」 | `WOW6432Node\…\Run` 的启用状态存在 `StartupApproved\Run32`，此前读/写都落在 `Run` 上 | 新增 `startup::approved_leaf()`，扫描与执行共用；写错子键会导致「显示禁用成功、重启照旧自启」的静默失效 |
| 点了「禁用」只弹出「拒绝访问 0x80070005」 | 演练阶段只算撤销载荷、不探权限，确认清单承诺「可一键还原」，执行时才失败 | `startup_ctl::probe_writable()` 在**演练阶段**只读探测服务 ACL / 注册表写权限，确认清单里把改不动的条目标黄并写明原因 |

**v0.1.3：让列表自己排好队**

真机上 143 个启动项里，只有 28 个是「可以勾」的，剩下 31 个已禁用、84 个不建议禁用。
旧版按系统原始顺序铺开，用户得自己在 143 行里找那 28 行 —— 这不是「全选不好使」，是**列表没帮人筛选**。

| 改动 | 做法 |
|---|---|
| 默认按「该不该动」排序 | `displayRank()` 给出 4 档：推荐禁用(0) → 可勾选(1) → 不建议禁用(2) → 已禁用(3)，在 `visibleStartups()` 里做**稳定排序**（同档内保持系统原序，不抖） |
| 已禁用可折叠 | 筛选栏「隐藏已禁用」开关，偏好写入 `localStorage`（`old666.hideDisabled`），下次打开沿用 |
| 隐藏不藏数字 | 副标题追加「· 已隐藏 N 项已禁用」，143 → 112 行时用户知道少的 31 行去哪了 |
| 隐藏再显示不跳过校验 | 重新显示时若仍有未补签条目，会重跑 `verifyStartup()`，不会因为「刚才藏过」就跳过签名校验 |

排序只影响**显示顺序**，不改变扫描结果、不改变「可勾选」的判定 —— 断言里专门验了这一点
（`整张表按「推荐 → 可勾选 → 不建议 → 已禁用」排列` 拿的是 DOM 里真实的 `data-rank` 序列）。

**v0.1.0 发布包验收**（`tools/measure-p0.py`，release 真机实测）：

| 指标 | 实测 | 验收 |
|---|---|---|
| `old666.exe` 体积 | **2.82 MB**（CLI 597 KB） | ≤ 12 MB ✓ |
| 冷启动到可交互 | 中位 **653.8 ms** / 最大 774.5 ms | ≤ 1000 ms ✓ |
| 完整扫描 | 2.96 ms（279 进程） | ≤ 200 ms ✓ |
| 便携包 | 解压即用，`data\` 就地生成，管理员 + SeDebugPrivilege + PDH 全绿 | ✓ |

> **P0/P1 的结论**：「结束进程」并不是产品价值的主战场 —— 255 个进程里真正可结束的只有 1 个，
> 而**工作集裁剪**有 56 个候选、合计约 1.1 GB。UI 文案不得把「净化」夸大成大幅提速。
>
> **P2 的结论**：判定质量与首屏速度不冲突 —— 拆成「快速扫描 + 按需补签」两步，
> 等价性由测试 `fast_then_verify_equals_deep` 保证；601 MB 的签名哈希是磁盘 I/O 瓶颈，
> 加线程只会更慢。延迟启动与禁用是**两种用户意图**（想留但想更快 vs 开机不再拉起），
> UI 必须拆成两个按钮、两套确认文案。另外，`[hidden]` 被作者样式的 `display` 盖掉这种事会反复咬人
> （抽屉常驻吃点击、刷新按钮该藏不藏），已在 `styles.css` 根部统一收口并加回归断言。
>
> **v0.1.0 的结论**：第一版就做到 2.8 MB / 冷启动 <0.8 s —— 便携与启动速度没有冲突，
> 前提是不引入 Electron 那类运行时；归因精度靠「用 Windows 自报数字」而不是自己估算。

详见 [`docs/实施计划.md`](docs/实施计划.md)、[`docs/P0-P1-技术验证报告.md`](docs/P0-P1-技术验证报告.md)
与 [`docs/P2-开机加速-技术说明.md`](docs/P2-开机加速-技术说明.md)。

---

## 安全设计（核心）

本产品会结束用户电脑上的进程，因此安全是**第一设计目标**，不是附属功能。
P0/P1 期间在真机上实测出 **74 个进程被误判为「可结束」**（含 `WorkBuddy.exe` 875 MB、
`iFlyInput.exe` 输入法、`MpDefenderCoreService.exe` Defender 组件、`chrome.exe` ×9、
`ScreenConnect` 远程控制端），因此判定引擎被**推倒重建**，从「按进程名猜」改为「按结构判」。

### 一、结束进程的四道闸门

| 闸门 | 位置 | 作用 |
|---|---|---|
| ① 硬黑名单 | `core/blacklist.rs` | 237 进程名 + 14 前缀 + 47 服务 + 46 驱动 + 6 系统目录。命中即 `Forbidden`，**不参与打分，白名单也解锁不了** |
| ② 判定封顶 | `core/idle.rs` | 8 条封顶规则，任意一条命中就把 `Safe` 压回 `Keep`（详见下表） |
| ③ 三层安全闸 | `core/safety.rs` | `check_target` → `check_execute` / `check_trim`，**每个写操作强制过闸**，防 TOCTOU |
| ④ 演练模式 | `core/safety.rs` | `PurgeMode::DryRun` 走完整判定 + 全部安全检查 + 收益估算，但**零系统写入、不落快照** |

### 二、8 条判定封顶规则（任意命中即降级为「保留」）

| # | 规则 | 拦截的真实误判 |
|---|---|---|
| 1 | 前台进程 | 用户正在用的窗口 |
| 2 | 持有可见窗口 | 常规前台应用 |
| 3 | **持有任意顶层窗口（含隐藏）** | 托盘应用、输入法、常驻助手 |
| 4 | **祖先链上有窗口进程** | Chrome 渲染进程、IDE 下的 node |
| 5 | **父进程是 shell（explorer）** | 用户从资源管理器启动的程序 |
| 6 | **父进程仍存活** | 只是「暂时安静」的正常程序 |
| 7 | **连续 N 轮观测到空闲** | 单轮采样抖动导致的误判 |
| 8 | 刚启动（< 300 s）/ CPU ≥ 2% | 启动中、正在干活的进程 |

> 这 8 条让真机上的「可安全结束」候选从 **74 → 9 → 2 → 1**。
> 详细实测见 [`docs/P0-P1-技术验证报告.md`](docs/P0-P1-技术验证报告.md)。

### 三、内置熔断器

- **测试熔断**：`cfg!(test)` 下演练模式**编译期强制开启**，任何单元测试都不可能真的杀进程。
- **PID 白名单闸**：`set_kill_allowlist(Some(pids))` 锁定本次操作范围，用完必须撤销；
  不在名单内的 PID 一律拒绝。
- **环境变量熔断**：`OLD666_DRY_RUN=1` 全局演练，`OLD666_DATA_DIR` 隔离测试数据目录。

### 四、结束进程必须人工确认（长期规则）

**任何结束进程的操作，都必须先把目标清单（PID / 名称 / 路径 / 判定 / 理由 / 内存占用）
摆给用户看，得到明确确认后才执行。** 该规则已固化为项目技能
[`.workbuddy-ai/skills/kill-process-confirmation/SKILL.md`](.workbuddy-ai/skills/kill-process-confirmation/SKILL.md)，
并在 PoC 中落实为 `--allow-write` 默认关闭。

### 五、其他保守约定

- 拿不到可执行路径的进程按「无法判定」处理，而不是当成「无签名」抬高可疑度
  （后者会诱导误杀系统组件）；
- 工作集裁剪不受进程名黑名单约束（裁剪不终止进程），但**同样受安全闸与交互判定约束**；
- 每次真实净化落一份 JSON 快照，可一键把被结束的进程重新拉起；
  演练记录**不计入可回滚集**，避免回滚时拉起从未被结束的进程。

### 六、P2 启动项变更的安全边界

启动项的「禁用」比结束进程温和，但同样是写操作，且更容易误伤（输入法、显卡驱动、
Defender 都藏在启动项里）。因此：

| 闸门 | 位置 | 作用 |
|---|---|---|
| ① 风险分级 | `core/startup_risk.rs` | 装备忘词（约 60 条：Defender/加密/音频/输入法/触控板/GPU 驱动/RPC/DHCP/打印/搜索…）**永不判为可安全禁用**；危险档是唯一不可禁用的等级 |
| ② 可疑特征只降不升 | `core/startup_risk.rs` | `\Temp\`、`-encodedcommand`、`-windowstyle hidden`、`powershell`/`mshta`/`rundll32` 等把「可安全禁用」压回「谨慎」，但绝不升级为危险（避免把用户自己装的脚本骂成病毒） |
| ③ 变更白名单闸 | `core/safety.rs` | `check_startup_change`：条目 ID 必须在本次允许清单内 + 熔断器关闭，否则拒绝；清单用后即撤 |
| ④ 先演练后执行 | `src/boot.js` + `ipc.rs` | 点「禁用选中项」先调 `startup_plan` 拿真实演练结果，把「会动哪些、怎么还原」逐条摆出来，用户点头后才调 `startup_apply` |

撤销载荷**自包含**：注册表原值 / 服务原 `Start` 值 / 计划任务原启用状态都写进批次文件，
换机器、删了程序也能还原。关闭服务走 `ChangeServiceConfigW(SERVICE_DISABLED)`
而不是直接写注册表 —— 后者不会通知 SCM。

> 实测分级：148 个启动项里危险 101（自动服务默认保守）、可安全禁用候选仅 11。
> 这与 P1 的结论一致：**宁可少关，不可错关**。

---

## 许可

许可证待定。

---

<sub>Brand mark © reloadbug. 商标形状 100% 保留，仅做矢量化与配色延展。</sub>
