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
    撤掉通知（已送达的和待送的都撤，包括上一次运行留下的）。
  - Sparkle 用 `respondsToSelector:` 问这些方法，所以 delegate 不需要声明 Sparkle 的协议（它在运行时才加载，
    编译时也声明不了）。
- **提醒用系统通知，不用托盘**：托盘菜单是 `catmail-frontend` 里固定的模型，加一个动态项改动大，也不显眼；
  Sparkle 文档的例子也是 UserNotifications。
- **不让发通知时退回 Sparkle 自己的窗口**：先用 `getNotificationSettingsWithCompletionHandler` 看授权，
  没授权（关掉了，或引擎没打开过、还没问过）、发送失败时直接 `checkForUpdates:`。不然更新没人看得见，
  比突然弹窗更糟。
- **从托盘打开窗口时也显示**：Sparkle 对 regular 应用就是"用户回来时显示"，这里跟它一致：`Window::present`
  从 accessory 切回 regular、而提醒还在时，调 `checkForUpdates:`。
- **通知和新邮件共用通知中心的 delegate**：UserNotifications 一个应用只有一个 delegate，已经在
  `system/new_mail/macos.rs`。按 identifier `update` 把点击转给 `shell::show_update`，不为一条通知另起
  一套通知中心。identifier 不会和新邮件的撞：那些都以 `new-mail` 开头。

## 方案

- `crates/catmail-gui/src/shell/appkit/updater/reminders.rs`：user driver delegate（`CatmailUpdateReminders`）、
  发通知、撤通知、`came_back`。
- `updater.rs`：把这个 delegate 交给 `initWithStartingUpdater:`，和 feed 的 delegate 一起留在 `Updater` 里
  （Sparkle 弱引用 delegate）；`show()` 调 controller 的 `checkForUpdates:`，用 thread_local 找到启动的更新器。
- `appkit.rs`：导出 `UPDATE_REMINDER`、`show_update`；`Window::present` 从 accessory 切回时调 `came_back`。
- `new_mail/macos.rs`：点击按 identifier 分派。
- `Cargo.toml`：`objc2-user-notifications` 加 `UNNotificationSettings`，`objc2-foundation` 加 `NSArray`。
- 文档：`packaging.md` 的自动更新，`background.md` 的 macOS 通知。
