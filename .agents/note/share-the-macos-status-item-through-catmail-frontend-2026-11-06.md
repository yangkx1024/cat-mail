# macOS 状态栏项抽进 catmail-frontend

- 日期：2026-11-06
- 类型：重构

## 背景

catmail-nsh 在 macOS 上要有和 gpui 版一样的状态栏图标。gpui 版的 `tray/macos.rs` 只用 AppKit（`objc2-app-kit`），
不依赖 GPUI，按 `run-catmail-nsh-on-macos` 那条记录的打算，应先抽进 frontend 再给 nsh 用，和 Linux 的
StatusNotifierItem（`tray::linux`）一样两版共用。

## 决策

- 状态栏项整体搬到 `catmail_frontend::tray::macos`，只在 macOS 编译，`unsafe` 只对这个模块放行。
- 图标的像素格式定成 AppKit 位图要的预乘 alpha RGBA（`tray::Template`），不再在 macOS 模块里转：GPUI 画出的是
  直通 alpha 的 BGRA，用 `Template::from_bgra` 转；nsh 用 resvg 画出的本来就是预乘 RGBA，直接用。
  `Template` 和尺寸（`MENU_BAR_SIZES`）不加 cfg，转换的测试在每个平台都跑。
- `spawn`、`show` 改成同步函数（AppKit 调用本来就是同步的），gpui 版在自己的 `platform` 模块里包一层 async，
  与 Linux 那层一样。模板图 SVG 仍由各前端自己带（和 `badged` 的应用图标一样）。

## 方案

- 新增 `catmail-frontend/src/tray/macos.rs`（原 gpui 的 `tray/macos.rs`，取 `&[Template]`）；`tray/mod.rs` 加
  `MENU_BAR_SIZES`、`Template` 和测试；frontend 在 macOS 上依赖 `objc2`、`objc2-app-kit`、`objc2-foundation`。
- gpui：删掉 `tray/macos.rs` 和 macOS 的 objc2 依赖，`tray/mod.rs` 的 macOS `platform` 包装 frontend 的 `Item`。
- 文档：`docs/architecture/background.md` 的托盘一节。
- 验证：macOS 上 `cargo clippy --workspace --all-targets -D warnings`、全部测试通过（frontend 新增
  `templates_are_premultiplied_rgba`，gpui 的托盘测试照旧）。
