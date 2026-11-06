# 去掉 GPUI 版，nsh 版改名为 catmail-gui

- 日期：2026-11-06
- 类型：重构

## 背景

nsh 版已经和 gpui 版功能对齐（Linux、macOS）。nsh 的开发体验比 GPUI 好，catmail 现在需要的能力没有被它卡住；
正文放在 web view 里，遇到的兼容问题也比自己用 Blitz 画 HTML 少。两个桌面端同时维护不值得，只留 nsh 版。

## 决策

- **catmail-frontend 保留为独立 crate，不并进 GUI**：它的一百多个测试不用编 GTK、WebKit、nsh；改界面不重编它；
  crate 边界保证模型层里没有 UI 库的类型；以后再换 UI 库也还能用。
- **nsh 版改名为 `catmail-gui`**（crate、二进制、目录）：打包、`.desktop`、Info.plist、release 里用的都是这个名字，
  crate 名也不再带 UI 库的名字。应用 id 从 `net.yangkx.catmail.nsh` 改回 `net.yangkx.catmail`，与 `.desktop`
  文件、macOS 的 bundle id 一致；调试用的环境变量 `CATMAIL_NSH_*` 改成 `CATMAIL_GUI_*`，日志 target 是 `catmail_gui`。
- **删除**：旧 `catmail-gui`（GPUI，连同自带的 Noto Sans 字体、fixtures、GPUI 测试）和只有它用的 `catmail-render`（Blitz）。
  workspace 去掉 gpui-kit、Blitz 全家、anyrender、parley、stylo、image、bytes 以及 Blitz 分叉的 patch，
  Cargo.lock 少了 518 个包，没有新增。frontend 里只给 GPUI 用的 BGRA 转换（`Pixmap::from_bgra`、`Template::from_bgra`）一并删掉。
- **图标**：nsh 版原来 `include_str!` 旧 crate 里的 Heroicons 和应用图标，搬到新 `catmail-gui/assets/`；字体不要了（nsh 用系统字体）。
- **打包**：macOS 的 DMG 改打 nsh 版（二进制名不变，AppKit、WKWebView 都是系统的，不再带字体许可）。
  Linux 的 AppImage 先撤掉（删 `build-appimage.sh`、`AppRun`，留 `.desktop` 和 appdata 给以后的包）：
  要把 WebKitGTK 带进去，它的辅助进程路径编译时写死，还有 bwrap 沙箱，工作量单独算，记进 ROADMAP。
  在那之前 Linux 只发 CLI 压缩包。
- **Windows**：暂时只有 CLI。nsh 在钉住的 rev 上有 Windows 后端（`nsh-platform-windows`，WebView2），
  catmail-gui 还没接，记进 ROADMAP。
- 主题文件仍是 GPUI Kit 的 JSON 格式（用户导入、主题文件夹里存的就是它），GUI 把它的颜色映射到 nsh 的 shadcn token，
  frontend 里讲这种格式的注释不动。

## 方案

- `git mv crates/catmail-nsh crates/catmail-gui`；代码里"as the GUI's (`旧文件`)"这类指向已删文件的注释去掉或改写。
- 根 `Cargo.toml`、`deny.toml`（去掉只经由 gpui-kit 的 advisory 忽略项和 Blitz 的 git 源，剩下的按现在的来源写理由）、
  `flake.nix`（只留 GTK 4、WebKitGTK；去掉 GPUI 的运行时库和 CJK 字体）、`ci.yml`、`release.yml`、
  `packaging/macos/build-dmg.sh`、`.gitattributes`。
- 文档：README、AGENTS.md、`docs/architecture.md`、`docs/architecture/gui.md`（改写为 nsh 版的设计）及其他提到
  GPUI/Blitz/AppImage 的子系统文档，`crates/catmail-gui/ROADMAP.md`（从移植清单改成 catmail-gui 的路线图）。
- 验证：macOS 上 fmt、`clippy --workspace --all-targets -D warnings`、全部测试通过；本机跑 `build-dmg.sh`（ad hoc 签名）。
  Linux 靠 CI。
