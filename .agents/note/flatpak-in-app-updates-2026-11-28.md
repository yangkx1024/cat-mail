# Flatpak 的应用内更新：portal 的 UpdateMonitor

- 日期：2026-11-28
- 类型：功能
- Issue：#406 第 4 步

## 背景

#407 之后 Flatpak 从自己的仓库更新，但只有 `flatpak update` 和软件中心会去装；常驻的应用不知道有新版本，软件中心
在后台装好以后，跑着的还是旧版本，直到用户自己重启。macOS 上 Sparkle 会提示、安装并重启，Linux 要有对应的东西。

## 决策

- **用 Flatpak portal**（`org.freedesktop.portal.Flatpak` 的 `CreateUpdateMonitor`、`Update`、`Spawn`）：沙箱里默认
  能调，不用加权限；系统级安装的更新 polkit 对活动会话直接放行。直接用 zbus，不引入 `ashpd`：已经依赖 zbus 5，
  开机启动的 Background portal 也是这样调的；两边共用的部分（portal 给调用者建的对象路径、portal 离开总线的监听）
  提到 `system/portal_bus.rs`，测试用的私有总线提到 `system/test_bus.rs`。
- **按 flatpak 1.18.4 的 `flatpak-portal.c` 确认过的行为**：
  - 每半小时检查一次，第一次在建好监视器半小时后，计量网络上跳过；新建监视器不会提前检查，所以不做"检查更新"。
  - `Update()` 立即返回，结果在 `Progress` 里：要新权限是 `Progress` 里的 `NotSupported`，不是 `Update()` 的错误
    （#406 里原先这样写）；第一次会弹 Access 对话框，不让就记成 `AccessDenied`。
- **提示放在侧栏底部那一行**（Settings 和连接问题的那一行），不用横幅或对话框：更新不急，那一行本来就是放全局状态
  的。按钮文字按结果写（"Install update"、"Restart to update"），不写版本号：portal 只给提交。
- **主窗口没开时发系统通知**，点了打开主窗口，照 #405 的分法；有主窗口时不发。
- **重启在引擎关闭之后才 `Spawn`**：退出时停止应答后来的启动是不可逆的（丢掉接收端），先 `Spawn` 新实例会把它的
  请求交给正在退出的旧实例然后自己退出；放到最后，新实例收到"正在退出"，等旧的放开数据目录再接手。`Spawn` 失败
  只能退出并记日志。外壳不认识更新：`shell::quit_then(reason, then)` 是通用的"退出、引擎关闭后再做这一步"，退出
  取消或第二个信号时丢掉那一步；起初 `finish` 里直接判断 `QuitReason::Update`，退出原因就从日志用的标签变成了
  控制流程的东西（/simplify 改掉）。
- **按钮和通知的规则在 frontend**（`Update::button`、`Update::notice`），GUI 只管画和发。
- **portal 离开总线后一分钟重建监视器**，建不起来就停：不在日志里反复报同一个错。

## 方案

- `catmail-frontend`：
  - `system/update/mod.rs`：`Update`、`Failure`（含给用户看的话），非 Linux 的空实现。
  - `system/update/tracker.rs`：把提交和 `Progress` 变成 `Update`，单测。
  - `system/update/linux.rs`：监视器线程、`Updates::install`、`restart`（`Spawn` 带 `LATEST_VERSION` 和
    `CATMAIL_*`/`RUST_LOG`），测试用私有 dbus-daemon 上的假 portal：发现、安装进度、被拒、portal 重启后重建、
    `Spawn` 的参数。
  - `system/resident.rs`：`QuitReason::Update`。
- `catmail-gui`：
  - `system/update.rs`：进程级的状态（`Watched`），启动时开始监视（`main.rs` 的 `launching`），通知；
  - `mailbox/update.rs`：侧栏的按钮，失败时 toast；`buttons.rs` 加 `busy_labelled_button`（和 `labelled_button`
    共用外框）；
  - `shell/mod.rs`：`quit_then`（引擎关闭后再做的一步），`main_window_open`。
  - 无头测试 `tests/updates.rs`：按钮随状态变化，失败的 toast，按"Restart to update"后应用退出。它抓到过一个
    问题：按钮在视图线程上直接调 `shell::quit`，进程崩溃；现在经 `on_main`。
  - `buttons.rs`：转圈和图标的切换（`busy_glyph`）两个按钮共用，只在忙不忙变化时重建，百分比变化不会让转圈重来。
- 真机实测（本地 Flatpak、临时密钥签名的本地仓库、`--poll-timeout=10` 的 flatpak-portal、本地 Dovecot 的测试账户）：
  发现 v2、portal 的授权对话框、安装、重启后跑的是 v2 的提交，portal 重启后一分钟内重建监视器，没有主窗口时的通知
  点了打开主窗口。也看到：flatpak-portal 找不到能用的 Access 实现时第一次更新失败（见 background.md）；两个数据目录
  的实例同时开着时，通知点击去先占到 app id 的那个（已知）。
- 文档：background.md「更新（Flatpak）」、退出原因；packaging.md、gui.md、README、ROADMAP。
