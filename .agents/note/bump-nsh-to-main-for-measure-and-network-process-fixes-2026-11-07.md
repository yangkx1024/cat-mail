# 升级 nsh 到 main：GTK measure 警告和 WebKit 网络进程的修复

- 日期：2026-11-07
- 类型：修复

## 背景

workspace 钉在 nsh main 的 f8cfd1a 上。之后上游合了我们报的两个问题：

- longbridge/nsh#1048（修复 #1049）：GTK 4.22 上 `NshView::size_allocate` 不 measure 就 allocate 子 widget，
  `gtk_widget_measure` 警告刷屏。
- longbridge/nsh#1051：nsh 一直持有 `NetworkSession`，最后一个 web view 关掉后 WebKit 的网络进程（约 41 MB PSS）
  还留着。

nsh 还没发含这些修复的版本（最新 v0.4.0 早于 #966 的修复），继续用 git 依赖。

## 决策

- rev 升到 main 的 59091c88（比原来多 98 个提交），不挑单个提交：上游 main 是线性的，中间的修复也想要。
- 顺带拿到的、对 catmail 直接有用的修复：macOS 上从 Finder 拖进写信窗口的文件现在给的是路径 URL（#1092，原来是
  `file:///.file/id=…`，附件名会变成 `id=…`）；拖到正文输入框上的文件交给外层的 `on_file_drop` 当附件，不再把
  路径插进正文（#1086）；列表里持有焦点或打开着菜单的行滚出视口也不回收（#1102）；内置滚动条释放时不再在
  reactive runtime 外 panic（#1095）。
- nsh 的 Linux 后端为声音（#955）无条件依赖 gstreamer 的 crate，没有 feature 可关，所以开发 shell 加了
  `gst_all_1.gstreamer`、`gst-plugins-base`，CI 的 Linux job 加装 `libgstreamer1.0-dev`、
  `libgstreamer-plugins-base1.0-dev`。运行时 WebKitGTK 本来就依赖 GStreamer，不多出运行依赖。
- gui.md 里"输入框失焦"写成了 longbridge/nsh#248，其实是本仓库的 #248，一并改正。

## 方案

- 根 `Cargo.toml` 的四个 nsh 依赖换 rev，更新注释；`Cargo.lock` 随之更新（新增 gstreamer 系列等 crate）。
- `flake.nix`、`.github/workflows/ci.yml`、README 的构建依赖加上 GStreamer。
- ROADMAP P10 勾掉 #1048 那一项，P9 的网络进程一项改成"已升级，待 Linux 上确认"；gui.md 的已知问题、
  background.md 的说明同步。
- 验证：macOS 上 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo test --workspace` 通过。本机没有 Nix 和 Linux，Linux 的构建、警告消失和网络进程退出留给 CI 和 Linux
  机器确认；`cargo deny` 本机未装，由 `audit.yml` 检查。
