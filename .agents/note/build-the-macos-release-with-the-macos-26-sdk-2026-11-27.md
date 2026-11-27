# macOS 发布包改在 macos-26 上构建，窗口用新系统的样式

- 日期：2026-11-27
- 类型：修复

## 背景

装的发布包（0.1.0）在 macOS 27 上红绿灯、工具栏还是旧样式，和系统自带的 app（Mail 等）不一样；本机
`cargo run` 的是新样式。`otool -l` 看 `LC_BUILD_VERSION`：发布包 `sdk 15.5`，本机构建 `sdk 27.2`。
AppKit 只给链接 macOS 26 以上 SDK 的 app 画新样式（Liquid Glass 的窗口按钮、unified 工具栏的尺寸），
旧 SDK 链接的 app 在新系统上按兼容走旧样式。发布 workflow 的 macOS job 在 `macos-15` 上跑，默认 Xcode
带的是 15.5 的 SDK。

旧样式下按钮的位置、工具栏的高度也和新样式不同，`chrome.rs` 里按新样式量的 52 pt 按钮行在发布包里会对不齐。

## 决策

- 改 `runs-on: macos-26`，用镜像默认的 Xcode。考虑过留在 `macos-15` 上 `xcode-select` 选 Xcode 26：要写死
  Xcode 的路径，镜像更新时还得跟着改，不如直接换镜像。
- 最低系统不变（`MACOSX_DEPLOYMENT_TARGET=12.0`、`LSMinimumSystemVersion`）：SDK 只决定链接时用哪套样式，
  不影响能在哪些系统上跑。

## 方案

- `.github/workflows/release.yml` 的 `macos` job 改 `runs-on: macos-26`，注释写原因；
  `docs/architecture/packaging.md` 跟着改。
- 验证：下次发布后在 macOS 27 上看窗口样式，并用 `otool -l` 确认二进制的 `sdk` 是 26 以上。
