# 用 nsh-testing 无头驱动 GUI 窗口

- 日期：2026-11-22
- 类型：功能

## 背景

GPUI 版删掉后，catmail-gui 没有驱动窗口的测试（ROADMAP P10 的 nsh-testing 一项）：读信、写信、设置、文件夹
的改动只能手动在应用里试。nsh 0.6.1 有 `nsh-testing` 的 `Headless`：真实的引擎和布局、不开窗口，按用户的
步骤（按、打字、右键菜单、回答对话框）驱动视图。

## 决策

- **外壳换成无头的工具包，而不是只测单个视图**。视图经 `shell::on_main` 请主线程开写信窗口、关窗；只起
  `mailbox::App` 的话写信窗口开不出来。测试构建里 `shell/` 用第三个工具包 `headless.rs`（和 `gtk.rs`、
  `appkit.rs` 同一套接口，`View` 是新的类型别名）：主线程是测试起的线程，自己跑循环（`async-task` 跑 future，
  `invoke` 和定时器排队），外壳开的每个窗口起一个 `Headless` 交给测试。开写信窗口、关窗询问、存完再关、退出、
  常驻都走真实的外壳代码。考虑过在 `shell::opened` 里加测试钩子，放弃：只绕过一处，关窗和退出就测不到。
- **测试一个接一个跑**：引擎、设置、托盘的请求是进程级的（和应用里一样），`Desktop` 持一把锁，开始时复原
  外壳的状态（`shell::reset`）。主线程的队列带着运行编号，失败的测试留下的线程收尾时不会清掉下一个的。
- **GTK / AppKit 的工具包在测试构建里照样编译**（`#[allow(dead_code)]`）：`gtk.rs` 的测试还要跑，
  `--all-targets` 也还检查它们。`gtk.rs` 里子进程测试的名字改成由 `module_path!()` 算出。
  `new_mail/linux.rs` 原来经 `shell::spawn` 用 GLib 的循环，测试构建里那变成无头的循环，它的测试就收不到
  通知；它本来就是 GTK 专用的代码，改成直接 `glib::spawn_future_local`。
- **选择器用用户看得见的名字**：给没有名字的输入框加无障碍标签（写信的 To / Cc / Bcc / Subject、`form::field`
  用它显示的标签、命名对话框用它的标题），读屏本来就需要；富文本编辑器自己不收标签，外面包一层带
  `test_id("message")` 的 view；读信区加 `test_id("reader")`，把标题和列表行分开。
- **客户端在后台同步、发信**，和应用一样，窗口里的改动自己到达服务器，测试只等最终状态，不手动 replay。
- 依赖：dev-dependency `nsh-testing`、`nsh-engine`（不带 JS）。它打开 nsh-core 的 HTTP / WebSocket，带进
  `webpki-roots`（CDLA-Permissive-2.0），只在测试里，`deny.toml` 对这一个 crate 放行。

## 方案

- `crates/catmail-gui/src/shell/headless.rs`：无头工具包、`Desktop`（起应用、等窗口、关窗、退出）、`TestWindow`。
- `crates/catmail-gui/src/tests/`：`TestMail`（假 IMAP / SMTP、后台同步的客户端）和四组测试，共 12 个：
  - 读信：点开后已读（服务器上 `\Seen`）、归档后打开下一封、删到废件箱再在废件箱删要确认（取消不删）；
  - 写信：发新邮件（主题、收件人、正文、Sent 副本）、回复（`In-Reply-To`）、关窗留草稿并能再打开、退出时
    每个写信窗口都先存好（退出后重开数据目录读草稿）；
  - 设置：外观、字号快捷键、关窗后常驻再打开、关掉常驻后关窗退出、没有账户时从登录页登录；
  - 文件夹：侧栏菜单新建、重命名、删除（确认），服务器上跟着变。
- 文档：docs/architecture/testing.md 的 GUI 一节、gui.md、ROADMAP、AGENTS.md。
- 验证：`cargo test --workspace`、`clippy --all-targets -D warnings`、`cargo deny check` 通过；GUI 测试连跑
  5 次都过，全部约 9 秒。macOS 的编译和测试留给 CI。
