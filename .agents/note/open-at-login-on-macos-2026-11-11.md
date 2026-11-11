# macOS 开机启动与 `--background`

- 日期：2026-11-11
- 类型：功能

## 背景

background.md 早就设计了"开机时启动"和 `catmail-gui --background`，一直没做。先做 macOS：登录时打开 catmail，
不弹窗口，直接常驻同步。

## 决策

- 用 `SMAppService.mainApp`（macOS 13 起），出现在系统设置的"登录时打开"里。放弃了 `SMAppService.agent`：
  LaunchAgent 的 plist 能直接带 `--background`，但它列在"允许在后台"里，用户看不出是什么；也放弃了要带辅助
  应用的 `SMLoginItemSetEnabled`。
- 开关的状态以系统为准，不进 `settings.json`：用户也能在系统设置里改。只在开关上 `register`，启动时不调，否则
  用户在系统设置里关掉的会被重新打开。
- mainApp 的登录启动没有参数，只能看启动的 Apple Event。实测（macOS 27）没人打开应用时（shell、launchd 直接
  启动），AppKit 给自己发一个空的 `oapp`，发送方 PID 是自己；网上"登录项启动时没有 Apple Event"的说法因此不能
  照搬（`currentAppleEvent` 不是 nil）。规则：带 `keyAELaunchedAsLogInItem` 的是登录启动；自己发给自己的、
  登录项又开着的也算；Finder、Dock、`open` 打开的不算。实际登录时是哪一种还没确认，日志记
  `launched opened=… at_login=…` 供确认。
- 不开窗口的前提是开着"关闭窗口后保持运行"、引擎没失败：否则照常开窗口（关着保持运行时，从不开窗口的启动
  什么也留不下；引擎失败要窗口说明原因）。
- Linux 的 autostart（XDG 文件 / Flatpak 的 Background portal）留到下一步，`--background` 两边都已可用。

## 方案

- `catmail-frontend/src/autostart.rs`（+ `autostart/macos.rs`）：`available`、`state`、`set`（要批准时顺带打开系统设置）、
  `launched_at_login`；新依赖 `objc2-service-management`。
- `resident::at_launch`：启动时开不开窗口的纯函数。
- `catmail-gui`：`main.rs` 读 `--background`；`shell::launched` 在 GTK 的 `activate`、AppKit 的
  `applicationDidFinishLaunching:` 里决定开不开主窗口（AppKit 原来在 `app.run()` 之前就开窗口，现在挪到
  启动完成时，好读启动事件）；引擎失败时没有窗口就开一个说明原因（原来引擎打开期间关掉窗口会直接退出，
  现在也一样开窗口，关掉后退出）。Settings › General 加 "Open at login"，系统答复之前开关不接受按动（快速
  连按时两次改动可能在两个线程上先后颠倒，最后留下与用户选择相反的登录项；Codex 的 review 指出）；改动的
  线程比页面活得久，`autostart` 的 `set`、`state` 因此在全应用串行（一把锁）；页面打开时在另一个线程上读状态，
  读到之前开关不接受按动，重开的页面读到改动之后的状态，视图线程不等锁。
- 测试：`resident::at_launch` 的单元测试。开关的串行没有测试：要换掉系统调用才测得到 `set`、`state` 本身，
  只测那把锁等于测 `Mutex`；视图还不能无头驱动（P10）。手动：用临时数据目录分别以 `--background` 和普通方式运行
  `catmail-gui`，日志里前者 `launched without a window`、后者 `main window opened`，SIGTERM 都正常退出；
  shell 启动读到的事件是 `Unopened`。bundle 里的开关和真实登录还要在打包后的应用上试。
- 文档：background.md「开机启动」、`crates/catmail-gui/ROADMAP.md` P9。
