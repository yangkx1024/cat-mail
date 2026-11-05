# nsh 版的托盘、新邮件通知和关窗后常驻

- 日期：2026-11-05
- 类型：功能

## 背景

ROADMAP 的 P9：catmail-nsh 要像 gpui 版一样，关掉窗口后继续同步、发信、提醒新邮件，并能从托盘、通知或再次
启动回到窗口。设置里的 General 一节、P2 剩下的 Sync 按钮和 `actions.rs` 里还缺的快捷键也一起补上。共用的规则和
措辞已经先抽进 catmail-frontend（见同日的 `move-tray-notifications-and-resident-rules-into-catmail-frontend`）。

## 决策

- **常驻**：GTK 应用在最后一个窗口关掉后就结束，所以按 `after_last_window` 的结果 `app.hold()`，再开窗口时放掉。
  GTK 没有对应 GPUI headless 的做法，常驻时仍连着显示服务器，ROADMAP 里标了 ⚠️。重开时回到上次的位置：
  主窗口的 mailbox 每次换位置都记到 `shell::left_at`，下一个 mailbox 打开时取用。
- **从外面要主窗口做的事**（`shell::Ask`：转到某处、写新邮件）：每个视图跑在自己的线程上，所以用一个静态的
  通道。没有 mailbox 在听时，请求先存着，下一个 mailbox 打开时一次取走（`follow_asks`）。主窗口销毁时清掉
  发送端，免得请求落进一个已经不读的接收端。文件夹还没读完时，请求等第一次读完再做（`sidebar::Waiting`）。
- **通知**：nsh 没有通知 API，改用 GLib 的 `GNotification`。没装别的依赖，点击能回到应用的 action，
  fdo 后端还会带上激活令牌。tag 就是通知 id，合成的那条会替换前一条。考虑过直接用 zbus 调
  `org.freedesktop.Notifications`，但要自己听 `ActionInvoked`、自己处理激活令牌，没有采用。已知 GNOME 上点击
  无效（GLib 在那里走 GNOME Shell 的接口，要应用占着 D-Bus 名字），记在 ROADMAP。
- **托盘图标**：用 resvg 画。nsh-core 已经依赖 resvg，但没开 text，角标的数字画不出来，所以这里打开 `text` 和
  `system-fonts`，字体库只加载一次。亮暗直接读 portal 的 `color-scheme`，常驻时也跟着变。nsh 没有公开系统外观，
  它读 portal 的代码也是这个写法。
- **退出**：Ctrl+Q、托盘 Quit 和信号都走 `shell::quit`，先让写信窗口存好再关；有一个关不了就取消退出。补上
  SIGHUP；第二个信号直接退出。退出原因记进日志。
- **系统唤醒**：不听 logind，靠引擎自己发现睡眠，最多晚 30 秒。
- **Sync 按钮**放在搜索框左边，同步时用 nsh 的循环动画转圈，对应 gpui 版按钮的 loading。

## 方案

- 新增 `tray.rs`（计数、绘制、portal 外观，`TRAY` 只在 GTK 线程上用）和 `new_mail.rs`（攒批、重读、
  `send_notification`，注册 `app.open-mail` action）。
- `shell.rs`：`Ask` 通道、`show_at`、托盘命令（`chose`）、`window_removed`（常驻或退出，常驻后 2 秒
  `malloc_trim`）、`quit` / `exit`、信号。`engine.rs` 加了 `open()` 和 `usable()`。
- `sidebar.rs`：`go_to`、`compose`、`sync`、`syncing`；第一次读完文件夹时处理等着的请求。`list.rs`：`reveal`
  （用 frontend 的 `Reveal`）、Sync 按钮、搜索框的 ref（Ctrl+F）。`app.rs`：接收请求，绑 Ctrl+Shift+N、F5、Ctrl+F。
  `composer.rs`：Escape、Ctrl+Shift+A。`settings.rs`：General 一节放在最前面，与 gpui 版相同。
- 验证：新测试在各尺寸画出图标，并检查角标里有白色的数字，确认字体生效。用空数据目录启动，托盘项注册上了，
  图标出现在面板上；经 dbusmenu 点 Sync now 和 Quit，日志记 `quitting reason=tray`，引擎关闭。关窗后常驻、
  重开、通知点击和信号退出留给手动测试。
