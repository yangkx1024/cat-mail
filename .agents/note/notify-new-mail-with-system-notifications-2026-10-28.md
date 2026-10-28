# 新邮件的系统通知

- 日期：2026-10-28
- 类型：功能

## 背景

#106 的第四步（#82）：常驻之后关掉窗口也在同步，新邮件要有系统通知，点击回到窗口、打开那封信。
引擎的 `Event::NewMail` 已在 #148 实现，这里是前端的部分。

## 决策

- 通知放在应用级的全局 `Notifier`（`new_mail.rs`），引擎打开时由 `Resident` 启动，不属于任何窗口；
  点击后的窗口处理仍归 `Resident`（`Resident::show_at`）。
- "仍未读、仍在 INBOX"由核心判断：新增 `MailClient::unread_in_inbox`，与 `Messages::new_mail` 共用
  一个 SQL 条件（`UNREAD_IN_INBOX`），不让 GUI 自己去比 folder、标签。
- 攒 1 秒（`GATHER`）再说：同步按批发 `NewMail`，醒来后各账户同时同步；Linux 上 GPUI 不按 tag 替换
  通知，不攒会弹出好几条合成通知。三封以内逐封，多了合成一条。
- 点击后去哪里是一个 `Destination`（某账户的 INBOX 与那封邮件），由 `Mailbox` 按它读到的文件夹找 INBOX，
  与默认打开的收件箱一致；起初在 `Resident` 里点击时 `list_folders` 再找，多一次读，挑法也与
  `special_folder` 不同，改掉了。`Resident` 记着主窗口的 `WeakEntity<Mailbox>`，不经窗口的 Root 向下转型。
- 重读一次查询覆盖所有账户的 INBOX，顺序用 SQL 的 newest first，GUI 不再自己排；主窗口在前台时攒够就
  不再重读。
- 应用标识用数据目录的限定名 `dev.CatMail.CatMail`，没有另起一个。
- 加了设置"Notify about new mail"，默认开。
- Linux 上通知正文是标记，GPUI 不转义，由我们转义 `&<>`（只在 Linux / FreeBSD）。不支持 `body-markup` 的
  通知服务会照字面显示 `&amp;`；查能力要直接依赖 notify-rust 并多一次 D-Bus 调用，常见的通知服务都支持，
  不值得。
- 已知限制（GPUI Linux 实现）：点击时的激活令牌没有传出，Wayland 上窗口可能只被标成需要注意；
  通知收不回。

## 方案

- `catmail-store`：`Messages::unread_in_inbox(ids, inboxes)`，`new_mail` 改用共享的条件（`LazyLock` 里的 SQL）。
- `catmail-core`：`MailClient::unread_in_inbox(messages)`。
- `catmail-gui`：`new_mail.rs`（攒、重读、主窗口在前台时不发、生成通知、解析 tag）；`Resident` 记下窗口
  最后的显示环境、`main_is_active`、`show_at`；`mailbox::Destination`、`Mailbox::go_to`；
  `message_list::reveal` 在列表读到那封时选中它（记着点击后的第一次读的序号，早于点击开始的读没有它
  不算数；其间用户选了别的就放弃，`forget_reveal`）；`init_app` 里 `set_app_identity`；Settings › General
  加开关。
- 起初攒的任务存在 `Notifier` 里、到点时清掉，等于 drop 了自己（GPUI 的 Task drop 即取消），重读永远
  不返回；端到端测试发现后改成标志加 detach。
- 测试：store 的 `mail_still_new_is_unread_in_inbox_newest_first`；核心 `new_mail_is_what_arrives_in_inbox_unread`
  里补了 `unread_in_inbox`；GUI 的通知生成与 tag 单元测试，以及 `resident` 里三个端到端测试（关着窗口时
  通知并点击打开、窗口在后面时点击打开、窗口在前台时不通知），`reading` 里两个（等着要选的邮件时用户先
  选了别的，保持用户的选择；点击时正在进行的读早于邮件到达，之后的读到它仍打开它），以及正文转义的单元测试。
  没有在真实桌面上点过通知。
