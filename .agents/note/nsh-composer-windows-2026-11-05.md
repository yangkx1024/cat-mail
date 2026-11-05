# nsh 版的写信窗口

- 日期：2026-11-05
- 类型：功能

## 背景

`nsh-spike` 分支的 ROADMAP P5：nsh 版要有和 gpui 版一样的写信窗口，以及它的入口（Compose、回复、转发、草稿）。

## 决策

- **线程**：nsh 的每个 `AppView` 有自己的引擎线程，Rust 组件就跑在那里，不在 GTK 线程上。所以写信窗口的视图
  和 GTK 窗口之间要跨线程：视图要 GTK 做的事用 `shell::on_gtk`（`glib::MainContext::invoke` 加一个 oneshot），
  GTK 要视图做的事（关窗）走一个 `mpsc` 通道。按草稿登记的窗口（`OpenComposers`）在 GTK 线程的 thread_local 里。
- **把打开的草稿交给视图**：nsh 根组件的 props 只能是 JS 值，放不下 `Opening`。`Root::new` 的 props 闭包在视图
  自己的引擎线程上运行，就在那里把 `Arc<Start>` 放进该线程的 thread_local，组件再取出来。每个视图一个线程，
  所以不会串。
- **拦截关窗**：`close-request` 总是返回 `Stop`，把请求发给视图；视图按 `Lifecycle::close` 决定，存好后经
  `shell::composer_closed` 自己 `destroy` 窗口（`destroy` 不再经过 `close-request`）。视图已经没了（通道断了）
  就直接关。Ctrl+W 是 GTK 的 `window.close`，走同一流程。
- **退出**：Ctrl+Q（以及 SIGINT / SIGTERM）先 `close_all`，全部关掉后才关其余窗口；有一个关不了就不退出，和
  gpui 版一样。主窗口不再用"第一个窗口"去找，单独记住它，否则写信窗口在时，再次启动会把写信窗口当主窗口。
- **设置**：`shell::settings()` 原来是 thread_local，GTK 线程写入、组件在引擎线程读，读到的一直是默认值（P4 的
  "打开即已读"实际没读设置）。改成进程级的 `Mutex`。
- 写信窗口里的提示（附件加不上等）和主窗口一样用 toast；`app::own_tasks` 让写信窗口里起的任务（包括提示的计时）
  属于窗口的根 owner。
- From 有多个身份时点一下弹原生 `popup_menu`（带勾选），不用 nsh-base 的无样式 `Select`。
- 草稿行双击（`PressEvent::clicks == 2`）或 Enter 打开写信窗口，对应 gpui 版 List 的 confirm。

## 方案

- `crates/catmail-nsh/src/composer.rs`：入口（`open`、`new_message`、`reply`、`forward`、`bind_keys`）和视图
  （字段、正文、附件、底栏、拖放、From、跟随身份变化），状态是视图自己的 nsh 全局；时序全交给
  `catmail_frontend::composer::Lifecycle`。
- `draft.rs`：草稿的右栏（Edit、Delete draft、上传失败的提示、收件人、附件、正文）。
- `shell.rs`：`on_gtk`、写信窗口的登记 / 打开 / 关闭 / `close_all`、`show_view`（主窗口和写信窗口共用）、
  主窗口的弱引用、设置改为 `Mutex`。
- `reader.rs`：Reply、Reply all（`can_reply_all`，查不了时显示）、Forward；`sidebar.rs`：Compose；`commands.rs`：
  `primary_button`；`app.rs`：右栏按选中的行是邮件还是草稿切换；`list.rs`：`selected_draft`。
- 验证：nsh 和根 workspace 的 clippy、测试通过；截图确认 Compose 和回复按钮。写信窗口会建草稿并上传到服务器，
  没有无人值守的测试环境，交给用户手动试。
