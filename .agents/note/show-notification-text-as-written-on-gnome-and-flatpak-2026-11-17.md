# GNOME 和 Flatpak 里通知正文照原样显示

- 日期：2026-11-17
- 类型：修复

## 背景

`catmail_frontend::new_mail` 在 Linux 上总是把通知正文转义成标记（`&` → `&amp;` 等）。但应用用 GLib 的
`GNotification` 发通知，GLib 按优先级选路：沙箱里走 portal（`body` 是纯文本），有 `org.gtk.Notifications`
（GNOME Shell）时走它（正文不当标记读），都不是才走 `org.freedesktop.Notifications`（GLib 把正文原样交给通知
服务，服务按标记读）。所以 GNOME 上和 Flatpak（Linux 唯一的官方 GUI 构建）里，主题 `Q&A <draft>` 显示成
`Q&amp;A &lt;draft&gt;`（#331）。

## 决策

- 转义与否只有发通知的一侧知道，从不依赖 UI 库的 `catmail-frontend` 挪到 `catmail-gui/src/new_mail/linux.rs`；
  `Notice` 的正文改为各平台都是纯文本，macOS 不变。
- 照 GLib 的顺序判断：沙箱（`/.flatpak-info`、`SNAP`、`GIO_USE_PORTALS=1`）→ 不转义；`org.gtk.Notifications`
  有主人 → 不转义；否则问通知服务的 `GetCapabilities`，有 `body-markup` 才转义（与 nsh 自己的 Linux 通知相同）。
  问不到能力时转义，大多数服务读标记。
- D-Bus 调用异步（`call_future`），只在第一条通知前问一次并记住：GLib 也是在第一条通知时选一次路。问的期间
  要发的通知排队，答案回来后按顺序发。
- 没有改成绕开 GLib 自己发 freedesktop 通知：改动大，点击和 portal 的处理都要重做。

## 方案

- `crates/catmail-frontend/src/new_mail.rs`：删掉 `plain()`，测试改为正文在各平台都是原文。
- `crates/catmail-gui/src/new_mail/linux.rs`：`reads_markup()` 判断，`show()` 排队，`post()` 按结果用
  `glib::markup_escape_text` 转义。
- `docs/architecture/background.md` 写明规则。
- 验证：macOS 上 fmt、clippy、`catmail-frontend` 测试通过；本机没有 Linux 目标，`linux.rs` 没有在本地编译，
  也没在 GNOME、Flatpak、其他通知服务上实际看过，交给 Linux CI 和手动检查。
