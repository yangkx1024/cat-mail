# 按窗口和区域整理 catmail-gui 的文件结构

- 日期：2026-11-06
- 类型：重构

## 背景

nsh 版从 spike 起一个视图一个文件，全平铺在 `src/` 下，`list.rs`、`settings.rs`、`reader.rs`、`sidebar.rs`
都上千行，`commands.rs` 里混着邮件命令、各处共用的按钮和"Move to"对话框。改名为 catmail-gui 之后，按原来
GPUI 版的划分整理。

## 决策

- 仿 GPUI 版：主窗口各栏进 `mailbox/`（`app.rs` 成为 `mailbox/mod.rs`，三栏拆到 `panes.rs`；侧栏、文件夹命令
  `folder_commands.rs`、中栏 `list/`（行拆到 `rows.rs`）、邮件命令、`picker.rs`、草稿、发件箱）；读信区 `reader/`
  （`page.rs` 放进来，界面拆到 `view.rs`）；写信窗口 `composer/`（界面拆到 `view.rs`）；设置 `settings/`
  （Appearance、Accounts、Reading 各一个文件，General 小，留在 `mod.rs`）。
- 模块名沿用短名（`mailbox::list`、`mailbox::commands`），没用 GPUI 的 `message_list`、`message_commands`：
  代码里大量 `list::show`、`commands::run` 这样的调用不用改，只改 `use`。
- 通用按钮（`Size`、`icon_button`、`primary_button`、`text_button`、`separator`、`tooltip`）从 `commands.rs`
  拿出来成为 `buttons.rs`：写信、设置、侧栏都用，和邮件命令无关。`with_accounts` 移到侧栏，挨着它读的 `Accounts`。
- 外壳、托盘、`engine.rs`、`chrome.rs`、`theme.rs`、`form.rs`、`login.rs`、`toast.rs`、`icons.rs` 不动。
- 只搬代码，不改行为。`page.rs` 原来不受平台 `cfg` 限制，现在随 `reader` 只在 Linux、macOS 编译，
  它的测试在 Windows 上不再跑（纯字符串代码，与平台无关）。

## 方案

- `git mv` 保留历史，按段落标记（`// --- … ---`）拆文件；子模块用 `use super::{…}` 取父模块的私有项，
  父模块和兄弟模块要用的改为 `pub`；`cargo fix` 去掉拆分时多带的 import。
- `docs/architecture/gui.md` 的结构图和文中的文件路径同步更新。
- 验证：macOS 上 fmt、`clippy --workspace --all-targets -D warnings`、全部测试通过；Linux、Windows 靠 CI。
