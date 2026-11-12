# macOS 上的新邮件通知

- 日期：2026-11-12
- 类型：功能

## 背景

用户反映 macOS 上的 catmail 不弹新邮件通知，系统设置里 catmail 的通知开关是开着的。查下来是 macOS 上根本没接：
`new_mail` 模块只在 Linux 上编译，`shell/appkit.rs` 的 `engine_opened` 注明"system's need a bundle"，ROADMAP 里这一项
也没做。设置里的"Notify about new mail"在 macOS 上显示但不起作用。磁盘映像打包（`packaging/macos/build-dmg.sh`）
之后应用已经在 bundle 里运行，可以接上了。

## 决策

- **不用 nsh 0.5.0 的通知**。nsh 现在有了 Apple 平台的通知（`nsh-platform-apple` 的 `notifications.rs`），但发通知是视图
  根节点上的命令，点击也只回到那一个引擎；catmail 关窗后常驻时没有视图，主窗口也会关了又开。所以和 Linux 用
  `GNotification` 一样，由应用自己调 `UNUserNotificationCenter`。nsh 的视图保持 `AppOptions::notifications` 关着，nsh
  只在它开着时才设 delegate，不会跟我们抢。
- 用 `objc2-user-notifications` 的类型化绑定，没有像 nsh 那样 `dlopen` 后 `msg_send!`；版本与其他 objc2 crate 相同
  （0.3.2），只开要用的 feature。
- delegate 在 `NSApplication::run` 之前设好（`new_mail::install`）：Apple 要求在启动完成前设，否则点击通知启动
  应用的那次点击会丢。
- 通知的 identifier 用 tag，与 Linux 的 notification id 一样：合成的那条替换前一条，点击带回 tag。只认默认动作
  （`UNNotificationDefaultActionIdentifier`）。
- 应用在前台时也显示横幅（`willPresentNotification` 给 Banner | List | Sound），与"主窗口在前台时也通知"一致。
- 授权在引擎打开时请求（Alert | Sound），只有第一次会弹出询问；没请求 Badge，Dock 角标由托盘直接设。
- 带默认提示音，用户可以在系统设置里关掉。
- 没有 bundle identifier（`cargo run`）时碰通知中心会让进程崩溃，所以先查 `NSBundle::mainBundle().bundleIdentifier()`，
  没有就不订阅、不发，日志记一行。
- 攒、重读、点击后转过去这些两平台共用的部分移到 `new_mail/mod.rs`；原来直接用的 `glib::timeout_future`、
  `glib::spawn_future_local` 换成 `shell::sleep`（新加，用 `toolkit::after` 和 oneshot）和 `shell::spawn`。

## 方案

- `crates/catmail-gui/src/new_mail/`：`mod.rs`（订阅 `NewMail`、攒 `GATHER`、看设置、`unread_in_inbox` 重读、
  `clicked`）、`linux.rs`（原来的 `GNotification` 部分：`listen` 注册 `app.open-mail`，`show`）、`macos.rs`
  （`Delegate`、`install`、`listen` 请求授权、`show` 加 `UNNotificationRequest`）。
- `shell/appkit.rs`：`run` 里 `new_mail::install()`（只在这时查一次 bundle，通知中心和 delegate 存进 `OnceCell`）。
  两个工具包的 `engine_opened` 现在一样了，删掉，由 `shell/mod.rs` 的 `started` 直接启动通知、托盘和唤醒；`shell/mod.rs`
  加 `sleep`，macOS 上导出 `invoke`（点击从通知中心的线程回到主线程）；
  `main.rs` 在 macOS 上也编译 `new_mail`。
- 依赖：根 `Cargo.toml` 加 `objc2-user-notifications`，`catmail-gui` 的 `objc2-foundation` 加 `NSBundle`、`NSError`。
- 文档：`docs/architecture/background.md` 的系统通知一节、`gui.md` 的目录、README、ROADMAP。
- 验证：macOS 上 `cargo fmt`、`cargo clippy --workspace --all-targets -D warnings`、`cargo test --workspace`、
  `cargo deny check` 通过。Linux 的改动只是把原来的代码拆开，本机编译不了 GTK，靠 CI 的 Linux clippy 和测试。
  通知要在 bundle 里手动验证：收到新邮件弹出、前台时也弹、点击打开那封信、关窗常驻时点击、多封合成一条。
  通知中心没法在单元测试里用，没有加新测试；措辞、tag 和攒批已有 `catmail_frontend::new_mail` 的测试。
