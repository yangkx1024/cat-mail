# 让 catmail-nsh 在 macOS 上运行

- 日期：2026-11-06
- 类型：功能

## 背景

catmail-nsh 只有 Linux（GTK 4）后端，其他平台的 `main` 直接退出。想在 macOS 上试 nsh 的 AppKit 后端
（正文在 WKWebView 里）。nsh 两个平台的 `AppView` API 几乎一样，不同的是 catmail-nsh 自己写的外壳：
`shell.rs` 直接用 `gtk::Application`、GTK 窗口和 GLib 的主循环。

## 决策

- 外壳拆成共用部分（`shell/mod.rs`：设置与主题、主窗口和写信窗口的登记、窗口的 `Spec` / `Hooks`、退出流程、
  关窗后常驻、信号的处理）和两个工具包（`shell/gtk.rs`、`shell/appkit.rs`），工具包只提供窗口、主线程上跑 future、
  跨线程投递、定时、外观、引擎打开后的平台功能（GTK 上是托盘和通知）和主循环的结束。GTK 的应用一直 hold 着，
  什么时候结束由共用部分决定（`toolkit::end`），两边一致。没有复制一份 `shell.rs` 给 macOS：两边的退出流程和常驻逻辑必须一致。
- AppKit 没有 GLib 那样能跑 `!Send` future 的主循环：用 `async-task` 的 `spawn_local`，每次 schedule 往主
  dispatch queue 投一次 run（GPUI 的 macOS 也是这样）。定时用 dispatch 的 `after`。
- 窗口关闭：`windowShouldClose` 问写信窗口的视图（同 GTK 的 close-request），`windowWillClose` 停视图、清登记，
  再异步判断是不是最后一个窗口（主循环在 AppKit 的回调外停）。⌘Q、Dock 的退出走 `applicationShouldTerminate`，
  先取消，按 `QuitReason::Platform` 走自己的退出流程，主循环结束后照旧关引擎。点 Dock 图标唤回主窗口。
- 输入框失焦：nsh 的 macOS 后端和 Linux 一样，点别处时焦点留在输入框里，E、X、L 到不了视图。用本地事件
  monitor 看按下的位置，不在输入框里就把焦点交给视图的根（nsh 自己 blur 时也是这样）。
- 托盘（状态栏图标）和新邮件通知先只在 Linux：系统通知要求打包成 .app；状态栏图标可以搬 gpui 版的
  `tray/macos.rs`，但应先抽进 `catmail-frontend`（走 main），记在 ROADMAP P10。
- 快捷键提示用 nsh 的 `key_label`，传和 `on_key` 绑定时同一个按键串（Linux「Ctrl+N」，macOS「⌘N」）。
- 外观：标题栏跟应用的外观走，设在 `NSApplication` 上（窗口和系统面板都继承），视图的外观照旧由视图自己设。
- 第二个信号：先销毁所有窗口（视图在 thread local 还在时停掉），再结束主循环，两个平台一样。
- 菜单栏和停止主循环抄的是 nsh `run_app` 里的私有实现（`main_menu`、`finish_running`），外观的映射也是；
  输入框失焦在两个后端各写了一份。这些更适合放进 nsh，是上游的缺口（#248）。

## 方案

- `shell.rs` → `shell/mod.rs` + `shell/gtk.rs` + `shell/appkit.rs`；`on_gtk` 改名 `on_main`。GTK 的 `Window` 持弱引用：
  持强引用时窗口要等句柄都没了才 dispose，退出时在 thread local 析构里停 nsh 视图会 panic（冒烟测试发现）。
- `main.rs` 用 `nsh_platform` 别名指向两个平台 crate；`tray`、`new_mail` 只在 Linux 编译。`#![forbid(unsafe_code)]`
  去掉（workspace 的 lint 已经是 deny），只有 `appkit.rs` 放开。拖放附件的 `file:` URL 改用 `url` 解析（不再用 gio）。
- 依赖：根 `Cargo.toml` 加 `nsh-platform-macos`（同一个 rev，不开默认 feature）、`async-task`、`block2`、`dispatch2`；
  catmail-nsh 的 macOS 依赖加 `objc2`、`objc2-app-kit`、`objc2-foundation`、`ctrlc`。
- 验证：Linux 上 fmt、clippy、全部测试通过；用空数据目录启动、SIGTERM 退出，日志里窗口关闭、引擎关好、无 panic。
  macOS 只在 Linux 上交叉检查过（`cargo clippy --target aarch64-apple-darwin`，C 依赖用假编译器跳过），
  没在真机上跑过。
