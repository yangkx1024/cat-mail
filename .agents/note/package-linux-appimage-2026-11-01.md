# Linux 版打包为 AppImage

- 日期：2026-11-01
- 类型：功能

## 背景

还没有任何安装包，只能从源码构建运行。Linux 先发布 AppImage：一个文件、不用安装、各发行版通用，
支持增量自动更新。

## 决策

- **在 Ubuntu 22.04（glibc 2.35）上用 rustup 构建，不用 Nix。** Nix 打的包要么解释器和 RUNPATH 指向
  `/nix/store`，要么连 Nix 的 glibc 一起带上，而宿主机的 GPU 驱动和 Nix 的 glibc 对不上，Vulkan / GL
  加载不了。脚本检查解释器和 glibc 符号版本，dev shell 里构建的产物会被拒绝。
- **手写 AppDir，用 appimagetool 打包**，没有用 linuxdeploy：直接依赖只有 fontconfig、xkbcommon(-x11)、
  xcb 和 glibc，不需要自动收集库。只自带 libxkbcommon-x11 和 libxcb-xkb（只有 X11 用到，纯 Wayland
  的系统上可能没有），其余按 AppImage 的 excludelist 用系统的。脚本检查每个 `NEEDED` 都在允许的表里。
- **CLI 不放进 AppImage**（AppImage 只有一个入口），另出 tar.gz。
- **嵌入更新信息**（`gh-releases-zsync`），每次 release 上传 `.zsync`；不能用 prerelease。
- **release 全部 strip**（`strip = true`）：`catmail-gui` 从 86 MB 降到 69 MB，AppImage 约 42 MB。代价是
  panic 回溯只剩地址。
- appimagetool `1.9.1` 和 type2 runtime `20251108` 固定版本并校验 sha256，不用会移动的 `continuous`。
- **发布由手动运行的 workflow 改版本、打标签**，不靠推标签触发：GITHUB_TOKEN 推的标签不触发其他 workflow，
  所以改版本、构建、发布都在一次运行里。先推标签再构建，包里的 commit 就是发布的提交；放弃了"构建成功后
  才打标签"，那样构建用的是改版本之前的提交，日志里的 commit 是旧的，还带 `-dirty`。
- 打包时发现窗口没有设 app id，Wayland 合成器对不上桌面文件，任务栏显示通用图标，所以一起补上。

## 方案

- `crates/catmail-gui/src/layout.rs`：`window_options` 设 `app_id`（Wayland 在创建窗口时读），新增
  `open_window` 在构建时调 `set_app_id`（X11 的 `WM_CLASS`，GPUI 在 X11 上不读选项里的值）；主窗口和写信
  窗口改用它。
- `packaging/linux/`：`build-appimage.sh`、`AppRun`（把 `$APPDIR/usr/share` 加到 `XDG_DATA_DIRS` 前面，
  `fonts.rs` 从那里找 Noto Sans SC）、桌面文件、AppStream 元数据。
- `.github/workflows/release.yml`：在 main 上手动运行即发布。不填版本时取下一个 patch，也可指定版本；改
  `Cargo.toml` 和 `Cargo.lock`，提交并打标签，原子地推上去，再从标签在 x86_64 和 aarch64 上构建并创建
  release。改动打包的 PR 只构建、上传 artifact。
- 文档：新增 `docs/architecture/packaging.md`，README、`background.md`（开机启动要写 `$APPIMAGE`）同步更新。
- 验证：本机（NixOS，Wayland）用去掉 glibc / 解释器检查的脚本副本打包，`readelf` 确认 `.upd_info` 和
  `.zsync`；运行 AppImage，`WAYLAND_DEBUG` 看到 `set_app_id("net.yangkx.catmail")`，环境里没有 CJK 字体时
  仍加载到 Noto Sans SC（来自 AppImage）；Vulkan 正常选中 RADV。`appstreamcli validate` 和
  `desktop-file-validate` 通过。Ubuntu 22.04 上的正式构建由 PR 的 release workflow 检查。
  版本 job 的脚本在临时 clone 里跑过（`git push` 换成 echo）：不填得 0.1.1，只改 `Cargo.toml` 和 9 个工作区
  crate 的 lock 条目；`0.2.0`、`v0.1.0`（不改版本、只打标签）通过；`0.0.9`、`1.2` 被拒绝。
