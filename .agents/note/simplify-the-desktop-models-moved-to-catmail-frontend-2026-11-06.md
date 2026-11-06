# 精简移进 catmail-frontend 的桌面模型

- 日期：2026-11-06
- 类型：重构

## 背景

#241–#245 把列表、侧栏、命令、托盘、主题卡片的逻辑移进 catmail-frontend 之后，对整个分支跑了一遍 simplify（复用、
精简、效率、层次四个角度）。

## 决策

- **改了的**：
  - 命令：`Change` 带上 `Op`（归档、移动、删除、标记、标签），`Change::run` 自己调 `MailClient`，`failure`、
    `may_remove`、`unchecks` 说失败时的话、会不会离开列表、要不要去掉勾选（有测试）。`FlagChange`、`LabelChange`
    和 `Deletion.mode` 去掉；gui 只剩一个 `run_change`。`movable` / `move_targets` 改拿列表的 `Source`，自己算
    `shown_in`，gui 不再先算一遍再传回去。
  - 行上的标签名：原来每行读 `Accounts` 并按 id 在文件夹树里找（同步时每 200 ms 账户一变，每个可见行都重算），
    改为 gui 的一个 `ShownIn` signal（`Source::shown_in_each`，按账户算一次），换来源或账户变化时更新，没变不写。
    `Source::shown_in` 改为拿账户条目，文件夹用平铺的 `get`，不再建整棵树的节点列表。
  - 按 id 找账户写了八处：统一成 `AccountEntry::find`，gui 的 `with_account` 也用它；`Accounts::list` 和
    `sidebar::has_accounts` 去掉重复的 `a.0.as_deref().unwrap_or_default()` 和两份 `has_accounts`。
  - 列表：`ListModel` 不再另存一份来源，读取都从传进来的 `Listing` 取；`Removal::reselect` 直接返回 `Select`，
    `Reselect` 和 `reselect` 只在 `list` 里用；`fetch`、`Days`、`Removal`、`Reveal` 收为 crate 内可见，删掉没人用的
    `Days::len`。gui 的 `Reads` 和 `Typing` 合成一个 `ListState`，`reads(|model, listing| …)` 去掉重复的嵌套读取。
  - 侧栏：`Loaded` 的 `select: Option<Option<Place>>` 加 `refresh_list` 改成 `Selection::{Keep, KeepAndRefresh,
    Go}`；折叠账户用标题自己的 `open`（`set_account_open`），不再为此复制 `Folds`、带上整个账户列表。All Inboxes
    的未读数用 `Place::Inboxes.folders`；一个账户的地方和标签由 `places` 一起建。
  - `Rgb::parse_with_alpha` 一次读出 alpha，`Rgba::parse` 不再自己剥 `#` 再读一遍（`0x…aa` 的 alpha 原来会丢）。
  - 主题卡片直接用 `color(swatch.x)`，不再先装进数组再解构。
  - 测试：`fixtures::standard_overview`、`fixtures::midday`，list.rs 的测试改用 `fixtures::message`；托盘的图标测试移到
    `tray/mod.rs`。
- **没改的**：
  - 搜索框的防抖（按键计数、停顿后换来源）和服务器搜索的时机仍在 gui：是输入的节奏，搬进模型要另设计，不在这次范围。
  - `show_loading` 读 `ListModel::is_reading`、`Coalesce::is_running`：换成返回状态变化只是挪个地方。
  - `SidebarModel` 的 `finish_read` 单独调用：和读取失败时的通知放在一起更清楚。
  - folders.rs、outbox.rs、new_mail.rs 测试里各自的 `Folder`、`OutboxEntry`、`MessageSummary` 字面量：它们的取值
    （分隔符、账户号）和 fixtures 不同，改用 fixtures 要动这些测试的断言。
  - 侧栏 `heading()` 多次读 `g.heading`：不是这次改的代码。

## 方案

涉及 `catmail-frontend` 的 `commands.rs`、`mailbox.rs`、`mailbox/sidebar.rs`、`list.rs`、`list/model.rs`、
`theme/{palette,swatch}.rs`、`tray/{mod,model}.rs`、`fixtures.rs`，`catmail-gui` 的 `mailbox/{commands,sidebar}.rs`、
`mailbox/list/{mod,rows}.rs`、`settings/appearance.rs`，以及 gui.md 读与改一节。fmt、clippy、全部测试通过。
