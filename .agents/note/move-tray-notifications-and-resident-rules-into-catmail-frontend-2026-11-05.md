# 托盘、新邮件通知和常驻规则抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 版要做 P9（常驻与系统集成），需要托盘、新邮件通知、关窗后常驻，规则和措辞都要与 gpui 版相同。这些逻辑原来
都在 `catmail-gui`。其中不依赖 GPUI 的部分先抽出来，再给 nsh 版调用，不复制一份。

## 决策

- **托盘**（`catmail_frontend::tray`）：抽出菜单（`MENU`、`Command`、`Chosen`）、提示文字、角标文字和带角标的
  SVG（`badged::svg`）。应用图标由调用方传进来，frontend 不引用 gpui 版的 assets。Linux 的 StatusNotifierItem
  （`tray::linux`）整个搬过来：ksni 只用 D-Bus，不依赖界面库。图标统一用 `Pixmap`（ARGB、非预乘）传，gpui 版从
  BGRA 转，nsh 版从 RGBA 转。用 GPUI `SvgRenderer` 画图标、macOS 的状态栏项仍留在 gpui 版。
- **新邮件通知**（`catmail_frontend::new_mail`）：搬过来的有攒 1 秒（`GATHER`、`Arrivals`）、逐封还是合成一条
  （`notifications`，返回与界面库无关的 `Notice`）、正文转义、tag 与点击后的去处（`Target`）。gpui 版的 `Notifier`
  只剩订阅事件和发 `SystemNotification`。
- **去处**：`Destination` 从 gpui 版的 `mailbox` 移到 `catmail_frontend::mailbox`，并加上 `Destination::find`，
  在账户里找到对应的位置。
- **常驻**（`catmail_frontend::resident`）：`QuitReason` 和 `after_last_window` 搬过来。参数从 gpui 版的 `Engine`
  改成"引擎还能不能用"的布尔值。结果从 `GoHeadless` 改名为 `KeepRunning`，因为 GTK 关窗后不断开显示连接。
  `after_reconnecting` 只与 GPUI 切换显示模式有关，留在 gpui 版。
- **回收内存**：`memory::release_freed`（`malloc_trim`）搬到 frontend，libc 依赖跟着过来。frontend 的根原来是
  `forbid(unsafe_code)`，改成用 workspace 的 `deny`，由 `memory` 模块单独放行，与 gpui 版原来的做法相同。
- **列表的 reveal**（`catmail_frontend::list::Reveal`）：点击通知后，要等列表读到那封邮件才选中它。等到哪次读为止
  的规则抽出来，gpui 版 `message_list` 的 `revealing` 改用它。
- gpui 版不再直接依赖 ksni 和 libc。

## 方案

- 新增 `tray/mod.rs`、`resident.rs`。从 gpui 版 `git mv` 过来的有 `tray/linux.rs`、`tray/badged.rs`、
  `new_mail.rs`、`memory.rs`；`mailbox.rs` 加了 `Destination`，`list.rs` 加了 `Reveal`。
- gpui 版：`tray/mod.rs` 的 Linux 平台模块变成包一层 frontend 的 `Item`，应用图标常量留在这里；`new_mail.rs`
  只剩 `Notifier`；`resident.rs`、`mailbox/mod.rs`、`message_list/mod.rs` 改用 frontend 的类型。
- 测试：菜单、角标文字、`after_last_window`、通知措辞和 tag 的测试随代码搬到 frontend。新加的测试有像素格式转换、
  带角标 SVG 的宽度、`Reveal` 等到哪次读为止、`Arrivals` 一次攒一批。画图标（`SvgRenderer`）的测试和
  `Resident` 的集成测试留在 gpui 版，行为不变。
- 文档：background.md 写明各部分现在在哪里，同时更新了 AGENTS.md 和 architecture.md 的 crate 表。
