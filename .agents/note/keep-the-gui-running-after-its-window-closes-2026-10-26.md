# 关掉窗口后 GUI 继续运行，再次启动唤起它

- 日期：2026-10-26
- 类型：功能

## 背景

Issue #106（后台运行机制，#82 的一部分）。关掉主窗口就退出，后台同步、发信都停了；之后要加托盘和新邮件
通知，前提是关掉窗口后进程还在。常驻之后再从启动器点一次 cat-mail，原来会显示"数据目录被占用"，要改成
唤起已在运行的那个。设计见 `docs/architecture/background.md`。

## 决策

- **GUI 进程常驻，不拆守护进程**：守护进程要把 `MailClient` 整个经 IPC 暴露，macOS 的状态栏图标仍要 UI
  主循环；GPUI Fast 能在没有窗口时切到 headless（Linux 断开显示服务器，Windows 释放 GPU），常驻开销接近
  守护进程。先用原型在 Wayland 上确认：headless 期间定时器、D-Bus 通知照常，能切回窗口模式再开窗口。
- **引擎归应用**：`Resident` 全局持有引擎和主窗口；关掉主窗口时 `Mailbox` 整个释放，重开时新建，并回到
  关闭时的 `Place`。"关闭窗口后保持运行"默认开（Settings › General），macOS 不切 headless（保留 Dock 图标，
  经 `on_reopen` 唤起）。
- **单实例用数据目录里的 Unix socket**，三个平台一套代码（Windows 上用依赖树里已有的 `uds_windows`），
  不用 named pipe（要自己设 DACL）。谁是实例由引擎的锁决定：核心新增 `Config::lock` 与
  `MailClient::builder_locked`，GUI 在 `main` 里同步加锁、绑定 socket，再把锁交给引擎，迁移仍在后台。
  锁被占用就发请求，1 秒内没人应答就是 CLI 占着，照常启动、显示"数据目录被占用"。
  - 起初 GUI 另有一把 `gui.lock`（引擎在 `open_async` 里才加锁），还要公开 `Config::create_data_dir`；
    /simplify 的审查指出两把锁是在补症状，改成上面这样。
- **路径太长时 socket 放进运行时目录**：手动测试用了很深的 `--data-dir`，绑定失败（`SUN_LEN`），两次启动
  都打开了窗口，第二个显示数据目录被占用。路径超过约 100 字节时改放在 `$XDG_RUNTIME_DIR`（Linux，没有就
  不放，不用 `/tmp`；其他平台为每个用户的临时目录），名字取数据目录规范路径的 FNV-1a 哈希（不用
  `DefaultHasher`，它在不同版本间可能变）。绑定仍然失败时只记日志，之后的启动显示"数据目录被占用"。
- Codex review 修复：
  - socket 放哪里原先按 `absolute` 路径的长度决定，用很长的真实路径和用指向它的短链接启动的两个实例找不到
    对方（一个在运行时目录，一个在链接下）。改为先 `dunce::canonicalize` 再判断，新增链接的测试（去掉修复
    时失败）。
  - 从 headless 重连失败时，切换期间另一个启动的请求被丢掉，而它已经得到成功答复，应用停在 headless。改为
    失败后用那个请求的显示环境再试（纯函数 `after_reconnecting` 及测试）。
- 已打开主窗口时的唤起只能 `activate_window`：GPUI 不接受外来的激活令牌，Wayland 上可能只标成需要注意。
- CLI 打不开数据目录时提示应用关掉窗口后仍在运行。
- **退出由 `Resident` 一处决定**：原来退出状态在写信窗口模块里，由最后关掉的写信窗口 `cx.quit()`，
  `Resident` 还得读它的标志才不在退出途中切 headless。改为 `Resident::quit` 持有 `quitting`、绑 Quit
  action，写信窗口只提供 `close_all`（oneshot 答全部关掉 / 有一个关不了），托盘的 Quit 也走这里。

## 方案

- `catmail-gui/src/instance.rs`：`claim`（交给已在运行的实例，或加锁、绑定、在线程上 accept）、带版本的
  JSON 行协议、`Display`（请求方的显示环境和 Wayland 激活令牌）、`socket_path`。
- `catmail-gui/src/resident.rs`：`Resident`（引擎状态、主窗口、`Windowing` 切换、等切换结束的请求）、
  纯函数 `after_last_window`。`main.rs`：启动前 `claim`，`QuitMode::Explicit`，`MainWindow` 跟随引擎状态。
- `Mailbox::reopen_at`；`Settings::keep_running` 与设置里的 General 一节；`composer::close_all`
  取代 `composer::quit`，`Resident::quit`。
- `catmail-core`：`Config::lock`、`DataDirLock`、`MailClient::builder_locked`（原先 client.rs 里的加锁与
  建目录移进 config.rs）。`catmail-cli`：`open_failed` 的提示。
- 文档：`architecture.md`（数据目录、子系统表）、`architecture/background.md`、`architecture/gui.md`。
- 测试：core 1 个（先拿的锁归引擎）；instance 8 个（交给第一个、替换残留的 socket、等正在启动的、
  CLI 占着时交给引擎去报告、版本不同时答复、协议往返、长路径的数据目录、短路径与哈希稳定）；resident 4 个（关窗后的决定、主窗口关闭后引擎还在、`Mailbox`
  被释放、经请求重开且回到原来的位置、写信窗口关不了时取消退出）；写信窗口 2 个（全部关闭时存下输入并答
  "关完了"、有一个关不了时答"没关完"）；设置的 General 开关；CLI 的提示。
