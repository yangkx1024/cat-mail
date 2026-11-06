# 精简 catmail-frontend 与 catmail-gui

- 日期：2026-11-06
- 类型：重构

## 背景

catmail-gui 按窗口整理完文件结构后，对它和从没做过这类检查的 catmail-frontend 跑了一遍 simplify
（复用、精简、效率、层次四个角度），去掉重复和死代码，顺带修掉列表、读信区里几处多余的复制和重建。

## 决策

- **改了的**：
  - 确认对话框：六处各写一遍 `alert_dialog`，改成 `form::confirmed(Confirm)`；`Confirm` 带上按钮文字（`action`），
    `FolderDeletion` 改为包一个 `Confirm`，发件箱不再返回 `(Confirm, &str)`，删草稿的文案移到 frontend
    （`composer::draft_deletion`）。
  - 正文页面（CSP、注入脚本和消息协议、`body_page`）从 gui 的 `reader/page.rs` 移到 `catmail_frontend::html::page`，
    和"在浏览器打开"的页面共用一个 `escape`；页面测试又在 Windows 上跑了。删掉没人用的 `html::Carried`、`decode`，
    frontend 不再依赖 `data-url`。
  - 只在测试里或完全没人用的：`Days::get/flat/path`、`Coalesce::is_running`、`OpenComposers::windows`；
    `folder_tree`、`write_atomically`、`theme_set_json` 收窄可见性；FNV-1a 两份合成一个。
  - gui 里不属于主窗口的工具（`own_tasks`、`in_mailbox`→`in_view`、`off_thread`）移到 `tasks.rs`，`report` 移到
    `toast.rs` 并接受任何 `Display`；直接 `notify(Level::Error, …)` 的十来处改用 `report`，出错都记日志。
    `reader` 自写的 `write_off_thread` 改用 `off_thread`。
  - 重复的小块：按 id 找账户（`sidebar::with_account`）、选中的草稿 / 发件箱条目（`selected_row`）、右栏的返回按钮
    和字段行（`panes::back_button`、`fact`）、`shown_folders` 改用 `Place::folders`；`labelled_button` 移到
    `buttons.rs`，`Import`/`Candidate` 移到 `appearance.rs`，`Hovered` 移到 `rows.rs`。
  - 效率：行的 id 用 memo 取，指针划过行不再复制整封邮件摘要；行的两种形状只在"有没有打开的邮件"变化时重建，
    不是每次换选中；读信区工具栏两组按钮共用一个 `Applies`；正文页面存成 `Rc<str>` 放在单独的 memo 后面，
    读信区别的状态变化不再复制几 MB 的页面；"Move to" 按文件夹去重后再判断；侧栏更新一个账户只复制一次。
  - 内嵌图片只编码一次：`catmail_html::InlineImage` 改为持有编好的 `data:` URL（`Arc<str>`，私有字段）。URL 仍只能由
    catmail-html 生成（`InlineImage::new`）：清洗时不会再检查嵌进去的值，所以不能让调用方传进来。重做页面、在浏览器打开、页面加载完补发、
    新到一张时发给页面都用同一份，克隆也便宜，原来手写的 `copy_inline` 去掉。
  - `gui.md` 写明 Labels 不在三处共用的按钮里（只改一封、只在读信区工具栏、L 只对选中的那封），以及要三处一致
    先得定多封勾选时怎么改。
- **没改的**：
  - 列表的读取状态机（连同行改为下标或 `Rc`、`loading` 单独一个 signal）移到 frontend：更新进 #241。
  - 侧栏分组排序的模型和重读后的规则移到 frontend：更新进 #243。
  - "Labels" 并进 `MessageCommand`：会让悬停行和勾选工具栏也有它，属于行为变化，先在 gui.md 里写明现状。
  - 正文的 HTML 解析和改色放到主线程以外：要处理结果晚到、和页面显示时序的交互，等真卡了再做。
  - 托盘计数节流：一次 SQL，影响小，不做。
  - 正文显示与附件列表对 inline 部分的规则不一致（inline 但不是图片的部分两边都不收）：#249。
  - 发件箱右栏的 From 显示账户主地址而不是条目的发件身份：#250。

## 方案

- 涉及 `catmail-frontend`（`commands`、`mailbox`、`outbox`、`composer`、`accounts`、`html/`、`list`、`refresh`、
  `folders`、`settings`、`theme`、`lib.rs`）、`catmail-html`（`InlineImage`）和 `catmail-gui` 的多数视图；
  `docs/architecture/gui.md` 的结构图同步。
- 验证：macOS 上 fmt、`clippy --workspace --all-targets -D warnings`、全部测试（806 个）通过；Linux、Windows 靠 CI。
