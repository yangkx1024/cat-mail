# 常驻时用通知提醒 Sparkle 找到的更新

- 日期：2026-11-27
- 类型：功能
- Issue：#402（GUI 一节的最后一项；叠在 #404 上）

## 背景

#403 起了 Sparkle 的更新器，用的是它的默认显示方式。应用常驻时是 accessory 的（没有 Dock 图标、没有窗口），
Sparkle 把这样的应用当作后台应用：定时检查找到更新时，马上把窗口放在别的应用后面；刚启动时（比如登录时常驻
启动）还会激活应用、把窗口提到最前。用户既没打开应用，也没点检查，窗口突然出现，或者藏在后面没人看见。
Sparkle 日志里也会提醒："Background app automatically schedules for update checks but does not implement
gentle reminders"。

## 决策

- **用 Sparkle 的 gentle reminders**（`SPUStandardUserDriverDelegate`），按 Sparkle 2.10.1 的
  `SPUStandardUserDriver.m` 确认过行为：
  - `standardUserDriverShouldHandleShowingScheduledUpdate:andInImmediateFocus:` 只在 accessory 时答 NO，
    由应用处理；regular（有窗口）时答 YES。regular 应用不在前台时，Sparkle 自己就是等用户回到应用再显示，
    已经不打扰，不用改。`immediateFocus` 不看：常驻在登录时启动，正是 Sparkle 会给 immediate focus 的时候。
  - `standardUserDriverWillHandleShowingUpdate:forUpdate:state:` 在由应用处理时发一条系统通知，点了调
    `checkForUpdates:`，头文件写明这样会把这次更新提到最前。
  - `standardUserDriverDidReceiveUserAttentionForUpdate:` 和 `standardUserDriverWillFinishUpdateSession`
    撤掉通知（只有发过时才撤；没有 trigger 的通知立即送达，不会待送）。
  - Sparkle 用 `respondsToSelector:` 问这些方法，所以 delegate 不需要声明 Sparkle 的协议（它在运行时才加载，
    编译时也声明不了）。
- **提醒用系统通知，不用托盘**：托盘菜单是 `catmail-frontend` 里固定的模型，加一个动态项改动大，也不显眼；
  Sparkle 文档的例子也是 UserNotifications。
- **不让发通知时退回 Sparkle 自己的窗口**：没授权（关掉了，或引擎没打开过、还没问过）时通知中心拒绝
  添加，报 `UNErrorCodeNotificationsNotAllowed`；添加失败就直接 `checkForUpdates:`。不然更新没人看得见，
  比突然弹窗更糟。不另外先查授权：添加本身就会说，少一次往返。
- **从托盘打开窗口时也显示**：Sparkle 对 regular 应用就是"用户回来时显示"，这里跟它一致：`Window::present`
  从 accessory 切回 regular、而提醒还在时，调 `checkForUpdates:`。
- **通知中心提成共用的模块**（`shell/appkit/notifications.rs`）：UserNotifications 一个应用只有一个
  delegate，原来在 `system/new_mail/macos.rs`。提醒要发、撤通知，点击要转给更新器；照抄新邮件的发送代码
  会有两份，直接调用又会让 `shell`（更新器）依赖 `system`（新邮件），成了模块环。现在通知中心、delegate、
  授权、`post`、`withdraw` 都在外壳里，新邮件和提醒都用它。
  - 放在 `shell` 而不是 `system`：`system` 本来就依赖 `shell`，两边都能用；它也是平台的东西，和外壳的其他
    AppKit 代码在一起。
  - 点击按 identifier 分派：更新器启动时 `on_click(REMINDER, show)` 认领自己的，其余交给 `install` 时
    新邮件给的函数。通知中心不认识更新器，不然 `notifications` 和 `updater` 又互相依赖（deslop 报过）。
  - identifier 不会撞：新邮件的都以 `new-mail` 开头。

## 方案

- `crates/catmail-gui/src/shell/appkit/updater/reminders.rs`：user driver delegate（`CatmailUpdateReminders`）、
  发通知、撤通知、`came_back`。
- `updater.rs`：把这个 delegate 交给 `initWithStartingUpdater:`，和 feed 的 delegate 一起留在 `Updater` 里
  （Sparkle 弱引用 delegate）；`show()` 调 controller 的 `checkForUpdates:`，用 thread_local 找到启动的更新器。
- `shell/appkit/notifications.rs`：从 `new_mail/macos.rs` 搬来的通知中心和 delegate，加上 `post`、`withdraw`、
  `on_click`；`shell/mod.rs` 导出它。
- `appkit.rs`：`Window::present` 从 accessory 切回时调 `came_back`。
- `new_mail/macos.rs`：只剩新邮件自己的：`install` 把点击交给 `new_mail::clicked`，`show` 用 `post`。
- `Cargo.toml`：`objc2-foundation` 加 `NSArray`。
- 文档：`packaging.md` 的自动更新，`background.md` 的 macOS 通知。
