# nsh 版跟上 catmail-frontend 的清理

- 日期：2026-11-06
- 类型：重构

## 背景

catmail-frontend 拆分后的清理（PR #240 里的 2b64aba）改了几处公开接口，nsh-spike rebase 到 main 之后
catmail-nsh 编不过：`commands::picker_matches` 换成了 `picker_filter`，`Lifecycle::status` 改成返回
`Cow<'static, str>`。

## 决策

照 gpui 版在同一次清理里的改法改，两个前端行为保持一致：

- 文件夹选择器先用 `picker_filter` 把查询词转一次小写，再逐项比较。
- 保存草稿前先问 `Lifecycle::save_due`，不需要保存时不读表单。
- 丢弃草稿的确认文案改用 frontend 的 `discard_confirm`，不在 nsh 里再写一份。

## 方案

`crates/catmail-nsh/src/commands.rs`、`crates/catmail-nsh/src/composer.rs`（`Shown::status` 改为
`Option<Cow<'static, str>>`，显示时转成 `String`）。验证：catmail-nsh 的 clippy `-D warnings` 和测试通过，
根 workspace 的 fmt、clippy 和测试通过。
