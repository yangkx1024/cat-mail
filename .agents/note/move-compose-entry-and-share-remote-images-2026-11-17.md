# 写信入口移出 composer，「显示远程图片」只留一份

- 日期：2026-11-17
- 类型：重构

## 背景

#320 第 4、4b 项。`composer/mod.rs` 同时装着两个视图的代码：`open`、`new_message`、`reply`、`forward`、`choose_forward`、`bind_keys` 在主窗口的视图里运行，其余在写信窗口的视图里运行并读 `global::<Shared>()`（没有就 `expect`）。两边互相调用能编译、运行时 panic。阅读窗格和引文各写了一遍 `show_remote_images` / `add_remote_images`，只差 `RemoteImages` 放在哪、页面还在不在。

## 决策

- 6 个入口移到 `mailbox/compose.rs`，与主窗口视图的其他代码放在一起。
- 远程图片的逻辑放进 `web.rs`，两处各用一个 `RemoteImagesAt`（阅读窗格的 `Open(MessageId)`、引文的 `Quoted`）说明状态在哪、页面是否还要。`show_remote_images` 返回要跑的 future，由调用方决定跑在哪里：阅读窗格仍存进 `page_state().fetch` 以便取消，引文仍用 `in_window`。
- `update` 返回 `Option`，信号没运行闭包时退回原来的默认值（不取、不报错、不显示），不改行为。

## 方案

- 新增 `crates/catmail-gui/src/mailbox/compose.rs`；调用方（reader、sidebar、draft、list）改用 `compose::…`。
- `web.rs`：新增 `RemoteImagesAt`、`show_remote_images`、`add_remote_images`；`fetch_remote_images`、`fetch_ended`、`add_remote` 改为私有。
- 验证：`cargo clippy -p catmail-gui --all-targets -D warnings`、`cargo test -p catmail-gui` 通过；没有测试驱动窗口，未手动点过。
