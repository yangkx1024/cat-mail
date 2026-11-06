# 侧栏的模型抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 版要做侧栏（`crates/catmail-nsh/ROADMAP.md` 的 P2）。gpui 版侧栏的规则（文件夹分区、哪个引擎事件重读什么、
选中的位置、连接说明、文件夹右键菜单的项和文案）大多与窗口无关，按"纯逻辑共用，不复制"先抽出来。

## 决策

- `folders`：原 `mailbox/folders.rs`。图标从 `Hero` 换成与 UI 库无关的 `FolderIcon`（角色、标签、普通文件夹、
  All Inboxes、Outbox），GUI 的 `assets.rs` 用 `From<FolderIcon> for Hero` 对应到 Heroicons。
- `refresh`：原 `mailbox/refresh.rs`，`Source` 从 `message_list` 一起搬到 `mailbox`。`Coalesce::is_running`
  不再只在测试里编译，GUI 的测试跨 crate 要用。
- `mailbox`（新）：`Place`、`Source`、`AccountEntry`、`connection_note`、`Place::start`（原 `set_accounts` 里挑起始
  位置）、`Place::label`、文件夹右键菜单（`FolderMenuEntry`、`folder_menu_entries`、`folder_menu_label`、
  `new_folder_title`、`changeable_folder`、`folder_deletion`）、`SYNC_ON_OPEN_AFTER`、`REFRESH_INTERVAL`、
  `ALL_MAIL_HIDDEN`。`AccountEntry` 加了 `Clone`、`Debug`，跨 crate 后原来写在 GUI 里的 `impl AccountEntry`
  也只能搬过来。
- 对话框、`Destination`（通知跳转）留在 GUI，别的前端用到时再抽。

## 方案

- 新增 `catmail-frontend/src/{folders,refresh,mailbox}.rs`（前两个 `git mv`）；GUI 的 `mailbox/mod.rs`、
  `sidebar.rs`、`folder_commands.rs`、`message_list/mod.rs`、`assets.rs`、`tests/managing_folders.rs` 改为引用，
  行为不变。
- 文档：AGENTS.md 与 `docs/architecture.md` 的 crate 表，`docs/architecture/gui.md` 的结构。
- 验证：`cargo test --workspace`、workspace clippy、fmt 通过；文件夹分区和刷新规则的单测随文件搬进
  catmail-frontend。
