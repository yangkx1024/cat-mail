# Linux 托盘图标

- 日期：2026-10-29
- 类型：功能

## 背景

#106 的第五步（#107）：常驻之后窗口关掉了，要能从托盘看到有没有新邮件、回到窗口、写邮件、同步、退出。
设计早先定了 Linux 用 `ksni`（StatusNotifierItem）。

## 决策

- 托盘是应用级的全局 `Tray`（`tray/mod.rs`），与 `Notifier` 一样在引擎打开时启动；平台部分只负责把
  图标、提示、菜单交给系统（`tray/linux.rs`），Windows、macOS（#108、#109）接同一个接口。
- 菜单命令都交给 `Resident`：打开用 `show_at`（与点击通知相同），新增 `compose`、`sync`；Quit 直接调
  `Resident::quit`，不再"派发 Quit action"（没有窗口时 action 无处派发，调用同一个函数效果相同）。
- 写邮件在主窗口里做（`Mailbox::new_message`，用当前账户），窗口关着就先打开它、读完文件夹再写；
  考虑过不开主窗口、只开写信窗口，但要另外决定账户、在哪里报告失败，放弃。
- 后台同步（窗口关着时）失败只记日志：没有窗口可以报告，账户状态在回到窗口时显示。
- 未读数由核心给（`MailClient::count_unread_in_inboxes`，各账户 INBOX 的 `unread_count` 之和），不在 GUI
  里拿 `overview` 自己挑：`overview` 还要读草稿、发件箱、身份。
- 图标在运行时画：应用图标的 SVG 外面套一层，加角标和数字，用 GPUI 的 `SvgRenderer`（resvg）画成
  22 / 32 / 64 像素；不提交 PNG，也不另加依赖。数字靠系统字体（sans-serif），没有字体时只有红点。
- `ksni` 用 `async-io` 特性：依赖树里的 zbus 已经是 async-io（通知、密钥环），不为它引入 tokio。
  `cargo deny` 通过（Unlicense 在白名单里）。
- 激活令牌：ksni 能取到托盘点击给的 XDG 激活令牌，随命令传下去，`Resident::show_at` 因此多了参数；
  通知的点击仍没有令牌（GPUI 的限制）。
- 外观（亮/暗）用"System"主题同一个来源（`appearance::system`：窗口最后报告的桌面外观，GPUI 应用级的
  在 Linux 上不可靠），`Themes` 变化时重画；常驻期间没有窗口报告，变化要等下次开窗口。
- 应用标识（`APP_ID`、`APP_NAME`）从 `new_mail.rs` 移到 crate 根，通知和托盘共用。
- 托盘注册上了才开始数未读、订阅事件：GNOME 没装扩展、还没有托盘的 Windows / macOS 都不白读。这些平台上
  `Command`、`MENU` 没有用处，按平台 `allow(dead_code)`（带原因）；起初没有处理，macOS / Windows 的
  clippy `-D warnings` 会失败（codex 审查指出，在 Linux 上把平台条件改成假来模拟确认过）。
- 绘制一次一个（`Coalesce`）：起初每次重画各自在后台画、各自交给托盘，两次可能先后颠倒，旧的数字留在图标
  上（codex 审查指出）。
- 简化时没做的：把"窗口开着就做、否则记下"做成通用的请求队列（现在只有去处和写邮件两种，`in_main` 和
  `adopt` 已经把它们收在一处）；核心的"同步所有账户"（`Mailbox`、CLI、托盘各自循环，托盘那份不报告，
  合并要改 `Mailbox` 的报告方式）；角标数字用字形路径代替字体（首次画会加载系统字体库，一次几 MB）；
  计数按时间节流、并行同步各账户。

## 方案

- `catmail-core`：`MailClient::count_unread_in_inboxes`。
- `catmail-core`：私有的 `inboxes`，`unread_in_inbox` 与新的计数共用。
- `catmail-gui`：`tray/mod.rs`（计数、合并重数、画图标、菜单、处理命令；`platform` 模块按平台选，没有托盘
  的平台和测试里是空实现）、`tray/linux.rs`（`ksni::Tray`，BGRA 转 ARGB）；`Resident::compose`、
  `Resident::sync`、`Resident::in_main`、`show_at` 的激活令牌，`adopt` 拿到窗口、直接调 `go_to` /
  `new_message`（`Mailbox::reopen_at` 去掉了）；`mailbox` 导出 `Coalesce`、`Refresh` 供托盘复用。
- 测试：核心 `the_unread_in_inboxes_are_counted`；GUI 的菜单含每个命令一次、角标文字、各尺寸都画出且
  带角标的单元测试；`the_icon_shows_the_latest_count_however_its_drawings_finish`（测试里的托盘项记下每次
  的提示文字，连着两次重画，20 个调度种子；去掉串行化时种子 4 失败，留下旧的数字）；
  `resident` 里托盘计数随新邮件变化、从托盘打开关掉的窗口、窗口关着时写邮件（窗口和写信窗口都打开）、
  从托盘同步显示在窗口里，`a_composer_that_cant_close_calls_off_quitting` 改用托盘的 Quit。
- 手动：在 Wayland（waybar 类托盘）上用临时数据目录运行，用 `busctl` 确认注册到 StatusNotifierWatcher、
  Id / Title / Category / ToolTip / 三个尺寸的 IconPixmap / 菜单项正确，经 dbusmenu 点 Quit 后应用退出、
  图标消失。角标的样子导出 PNG 看过。没有真的用鼠标点过，也没有在 KDE、GNOME 上看过。
