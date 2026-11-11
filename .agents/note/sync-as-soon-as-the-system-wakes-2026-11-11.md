# 系统唤醒后马上同步

- 日期：2026-11-11
- 类型：功能

## 背景

GUI 换到 nsh 后没有 GPUI 的 `on_system_wake`，醒来只能等引擎自己发现睡眠（`sleep.rs` 每 30 秒比较一次
时钟），合盖再打开后最多要半分钟才换连接、同步、发到期的信。ROADMAP P9 的一条。

## 决策

- nsh 不转告系统唤醒，`catmail-gui` 自己听，只调 `MailClient::resume()`；取舍都在引擎里，引擎自己发现睡眠
  照旧保留作后备（`resume` 之后它不会再调一次，见 engine-notices-sleep-on-its-own-2026-10-28）。
- Linux 用 GIO 的 D-Bus 订阅系统总线上 logind 的 `PrepareForSleep`，只认参数为 `false`（醒来）的那次。
  ROADMAP 原先提到 zbus：它已在依赖树里，但要直接依赖、跑在 async-io 的执行器上再经 channel 回主循环；
  GIO 已随 GTK 进来，回调就在 GTK 主循环上，与托盘跟随 portal 外观的写法一样。不用 inhibitor：睡前不需要做事。
- macOS 用 `NSWorkspace` 通知中心的 `NSWorkspaceDidWakeNotification`，不给 queue（workspace 在主线程上发）。
- 放在 `catmail-gui` 而不是 `catmail-frontend`：Linux 一半用的是 GTK 主循环上的 GIO，搬到 frontend 就得换成
  zbus；这么小的功能分在两个 crate 里不值得，也没有可以抽出来测的逻辑。
- 引擎打开后开始听（`engine_opened`），直到退出；退出途中引擎已不可用时不调。
- Flatpak 加 `--system-talk-name=org.freedesktop.login1`，否则沙箱里收不到 logind 的信号。

## 方案

- `crates/catmail-gui/src/wake.rs`：`follow()`，Linux 与 macOS 各一份；`shell/gtk.rs`、`shell/appkit.rs` 的
  `engine_opened` 调用。`Cargo.toml` 给 objc2-app-kit 加 `NSWorkspace`，objc2-foundation 加 `block2`、`NSOperation`。
- `packaging/linux/net.yangkx.catmail.yml` 加权限；`docs/architecture/background.md`、`packaging.md`、ROADMAP 更新。
- 测试：`wake::tests::only_the_signal_after_the_sleep_is_a_wake`（Linux，只有 `(false,)` 算醒来）。`resume`
  本身由引擎的测试覆盖。没有在真机上测睡眠：醒来后日志应有 "the frontend says the computer woke"，且之后
  不再有引擎自己的 "the computer slept"。
