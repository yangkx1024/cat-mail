# nsh 版的应用骨架：自建 GTK 应用、引擎归应用、单实例

- 日期：2026-11-05
- 类型：功能

## 背景

catmail-nsh 对齐 gpui 版的 P1（`crates/catmail-nsh/ROADMAP.md`）。原来用 nsh 的 `run_app`：一个视图配一个窗口，关窗就结束；引擎是打开窗口前同步 `open()` 的，放在 `static` 里；也没有单实例。gpui 版的写信窗口、关窗后常驻，都要求窗口可以来去而引擎不动。

## 决策

- **自建 `gtk::Application`，每个窗口放一个 `AppView`**，照着 `run_app` 的做法来（`NON_UNIQUE`、prgname、Ctrl+Q / Ctrl+W，视图随窗口销毁而 `stop`）。没有用 GApplication 的 D-Bus 单实例：单实例交给与 gpui 版共用的 `catmail_frontend::instance`，这样两个前端谁先启动，谁就是这个数据目录的应用。
- **引擎归应用**（`engine.rs`）：状态是打开中、已打开、失败、已关闭。在 glib 主循环上 `open_async`；视图用 `opened()` 等它，等待者是 `futures_channel::oneshot`，与执行器和线程都无关。主循环结束后取出客户端，`shut_down`，再 `block_on(closed())`，和 gpui 版的 `Resident::shut_down` 一样等引擎关好。
- **SIGINT / SIGTERM 走 Ctrl+Q 的流程**，对齐 gpui 版（#212，那边用 `ctrlc`）。glib 0.22 把 Unix 信号拆进了 `glib-unix`，用它的 `unix_signal_add_local`，版本跟着 gtk4 用的 glib。
- **builder 选项与 gpui 版相同**：后台同步、预取正文、后台发送、GC。
- 设置只读不写：外观和文字大小在打开窗口时应用。写入要等 P7 有了设置对话框。`CATMAIL_NSH_APPEARANCE` 仍可覆盖外观，用来截图而不改设置。

## 方案

- 新增 `src/engine.rs`、`src/shell.rs`，改写 `src/main.rs`（claim、builder、设置）。`src/app.rs` 的根组件先显示空白，再换成邮箱界面或失败页（文案与 gpui 版的 `open_failed` 相同），重复的 `is_openable_link` 改用 `catmail_frontend::html`。
- `catmail_frontend::settings::DEFAULT_TEXT_SIZE` 改为 pub，用来算字号缩放。
- 验证（本机，真实数据目录）：
  - nsh 运行时，再启动 nsh 或 catmail-gui，都会把请求交给它后退出（约 0.4～1.6 s）。
  - SIGTERM 后日志依次为 "quitting on a signal"、"shutting down the engine"，exit 0，用时约 100 ms；进程的 `SigCgt` 有 SIGINT 和 SIGTERM。
  - 数据目录是普通文件时显示失败页和原因。
  - activation token 获取焦点没有单独验证：这台机器的合成器不是 niri 或 Hyprland，没有 IPC 可以检查。
