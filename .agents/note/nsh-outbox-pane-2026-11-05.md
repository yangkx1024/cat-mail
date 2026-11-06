# nsh 版的发件箱右栏

- 日期：2026-11-05
- 类型：功能

## 背景

`nsh-spike` 分支的 ROADMAP P6：发件箱条目的右栏，按状态给出 gpui 版那些选择。

## 决策

- 规则都在 `catmail_frontend::outbox`，nsh 版只画：标题、To / From / Queued、状态、按钮。
- nsh 主题没有 warning 色：未确认和被拒绝都用 destructive 色的框，只是图标不同（三角警告、叉），和侧栏发件箱的
  标记一致。
- 按钮第一个（最可能的选择）带边框，其余是无边框的文字按钮（`commands::text_button`），对应 gpui 版的
  outline / ghost。Send again 先用原生 `alert_dialog` 确认。
- 后台发送从 P1 起就开着（`send_in_background(true)`），`unknown` 不自动重发是引擎的规则，nsh 版不用另做。

## 方案

- `crates/catmail-nsh/src/outbox.rs`：右栏；`list.rs`：`selected_outbox`；`app.rs`：选中发件箱条目时显示它；
  `commands.rs`：`text_button`。条目随 `OutboxChanged` 引起的列表重读更新，右栏读的是列表里的那一行。
- 验证：clippy、测试通过。发件箱平时是空的，有条目的状态（尤其未确认、被拒绝）要真的发信才出现，交给用户手动试。
