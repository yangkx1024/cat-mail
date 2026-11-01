# 主窗口在前台时也通知新邮件

- 日期：2026-11-01
- 类型：功能

## 背景

新邮件的系统通知（见 `notify-new-mail-with-system-notifications-2026-10-28.md`）原先在主窗口处于前台时
不发。用户希望主窗口开着时新邮件也弹通知。

## 决策

- 不论主窗口在不在前台、开没开着都发；设置"Notify about new mail"仍可整体关掉。没有另加一个"前台时也通知"
  的开关。
- 攒 1 秒、发之前重读仍未读且仍在 INBOX 的、三封以内逐封，这些都不变。
- 顺带讨论过给通知带上应用图标：GPUI（gpui-fast）的 `SystemNotification` 没有图标字段，Linux 上只传
  `appname`，不传 `app_icon` / `image-data` / `desktop-entry`；notify-rust、user-notify 等三方库在
  macOS / Windows 上都不如 GPUI 自己的实现（user-notify 还是 LGPL-3.0、tokio 接口），暂时放弃。

## 方案

- `catmail-gui/src/new_mail.rs`：攒够后只看设置，去掉 `Resident::main_is_active` 的判断。
- `catmail-gui/src/resident.rs`：删掉不再使用的 `main_is_active`；测试
  `new_mail_isnt_notified_while_the_main_window_is_in_front` 改成
  `new_mail_is_notified_while_the_main_window_is_in_front`（窗口在前台时收到通知，点击打开那封信），
  用旧的前台判断跑会失败。`Notifier::is_gathering` 只有那个测试用，一并删掉。
- 文档：`docs/architecture/background.md`、README 的说明。
