# macOS 常驻时切到 Accessory 激活策略

- 日期：2026-11-11
- 类型：功能

## 背景

换到 nsh 之后，macOS 上关掉最后一个窗口进入常驻时一直是 `Regular`：Dock 图标和应用切换器里都还在。GPUI 版常驻时
切到 Accessory，只留状态栏图标（见 `macos-status-item-and-accessory-resident-2026-11-04.md`），ROADMAP P9 留着这一项。

## 决策

- 激活策略跟着"有没有窗口"走：常驻（`after_last_window` 的 `KeepRunning`、启动时 `AtLaunch::NoWindow`）切到
  Accessory，显示任何窗口（主窗口、写信窗口）时在 `Window::present` 里切回 Regular 再激活。放在 `present` 里而不是
  `shell::show`：所有显示窗口的路径（状态栏菜单、再次启动、引擎失败的说明窗口、写信窗口）都经过它。
- `--background` 启动在 `app.run()` 之前就设成 Accessory，Dock 图标不先出现再消失；登录时的启动要等
  `applicationDidFinishLaunching:` 读到启动事件才知道，bundle 的 Dock 图标会短暂出现，可以接受。
- 考虑过让策略只跟着 AppKit 自己的窗口列表走：一律以 Accessory 启动，`will_close` 发现没有窗口了就切 Accessory，
  去掉 `toolkit::resident`。打包试过（ad hoc 签名，单独的 bundle id）：普通启动时 Dock 图标先出现（Info.plist 让
  bundle 一启动就是普通应用）、被 Accessory 拿掉、开窗时又回来，每次启动都闪一下，放弃。只把关窗那半边挪进
  `will_close` 也不更简单：登录时的启动从来没有窗口，仍要在 `launched` 里切。
- 外壳共用部分只调 `toolkit::resident()`，GTK 上什么也不做：退出与常驻的决定仍只在 `shell/mod.rs` 一处。
- Dock 角标照旧在常驻时设置（frontend 的 `tray::macos` 本来就留着），回到 Regular 时数字就在。

## 方案

- `catmail-gui/src/shell/appkit.rs`：`run` 按 `HIDDEN` 选初始策略；新增 `resident`；`Window::present` 不是 Regular
  时先切回。`shell/gtk.rs`：空的 `resident`。`shell/mod.rs`：两处常驻时调用。
- 文档：`docs/architecture/background.md`「两个状态」、`crates/catmail-gui/ROADMAP.md` P9。
- 验证：没有可测的纯逻辑（激活策略是 AppKit 的状态），手动在 macOS 上用临时数据目录跑 `catmail-gui`，
  用 `lsappinfo` 看类型：`--background` 启动是 `UIElement`；再次启动唤起窗口后是 `Foreground`；用辅助功能点关闭
  按钮后回到 `UIElement`，窗口列表为空。从 Accessory 切回 Regular 后，用鼠标点菜单栏能正常展开（AppKit 已知的
  "切回后菜单栏不响应"没有出现），关窗后 Dock 图标消失，从状态栏菜单回到窗口正常。fmt、clippy、全部测试通过。
