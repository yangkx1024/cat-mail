# Gmail 标签的存储结构与 X-GM 协议支持

- 日期：2026-10-24
- 类型：功能

## 背景

[位置与标签](../../docs/architecture/labels.md)迁移步骤 4 的前半（#126）：数据和协议，不改同步流程。同步
（#127）、标签操作（#128）、GUI（#129）以它为基础。

## 决策

- **标签只显示"所有邮件"里的邮件**：移到已删除、垃圾邮件的邮件保留标签行（Gmail 移回时恢复标签），但
  标签和"已加星标"的列表、计数不算它们，与 Gmail 网页版一致。没有在真实账户上验证 IMAP 的标签邮箱是否
  也这样，写进了 `labels.md` 的"没有验证的"。条件集中在视图 `all_mail`，验证结果不同时只改它和列表查询。
- **"已加星标"不存标签行**（按设计）：列表走部分索引 `messages_flagged`，计数由 `messages` 上的触发器维护。
  考虑过用触发器自动维护 Starred 的标签行，让它与普通标签共用查询；但同步按 `X-GM-LABELS` 整体替换标签
  时要特意跳过它，反而多一处例外。
- **计数触发器合并成五个**：标签行插入 / 删除；邮件插入（Starred）、删除前（先删标签行，此时触发器还看
  得到邮件和它的位置）、`\Seen` / `\Flagged` / 位置变化时"按旧状态减、按新状态加"。文件夹删除前由
  触发器（`folders_delete_messages`）先删它的邮件，否则级联删除时位置已不在，触发器判断不出邮件原来在不在
  "所有邮件"里；最初在 `reconcile` 里手动先删，评审指出以后别的删除路径会忘，改到 schema 里。`all_mail`、
  `starred` 两个视图各有部分索引，否则每次 `\Seen` 变化都要扫一遍 `folders`。
- **文件夹的 `kind` 或角色变化、或新出现"已加星标"时重算整个账户的计数**：文件夹计的东西变了（位置的
  邮件 / 标签行 / 带 `\Flagged` 的邮件），增量触发器维护不了；很少发生，代价可以接受。最初只在 `kind` 变化时
  重算，评审（codex）指出"已加星标"在 IMAP 里隐藏过、邮件存下之后才列出时计数从 0 开始，取消星标后减成负数，
  读文件夹时按 u32 解码失败；角色变化同理。新增测试 `starred_listed_later_counts_what_is_flagged`，去掉修复
  后失败。位置变成标签时像 `\Noselect` 那样结束 UIDVALIDITY 代，
  标签变成位置时删掉它的标签行。账户模式本身只记录（`Accounts::set_mode`），按模式重建是 #127 的事。
- **标签随 flags 走**：`FlagState::labels: Option<Vec<GmailLabel>>`，`None` 表示没拉、不动已存的；
  `Messages::update_flags` 同时更新两者，#127 的 `CHANGEDSINCE` 拉一次就能存下 flags 和标签的变化。
- **`X-GM-LABELS` 的解析放在协议层**（`GmailLabel::System(role)` / `User { remote_name, name }`），因为用户
  标签名要按修改版 UTF-7 解码；映射到文件夹在存储层（`Folders::label`，同一批邮件里每个标签只查一次）：系统标签按角色，用户标签按
  `remote_name`，LIST 里没有的建一行 `listed = 0`；`\Starred` 和不认识的系统标签丢掉。新增
  `FolderRole::Important`（LIST 属性 `\Important`，RFC 8457）。
- **FETCH 的 `Items::Standard` / `Items::Gmail`** 由调用方传：只在 Gmail 账户的位置里取标签（在标签邮箱里
  答复不含它自己的标签），会话本身不知道当前邮箱是什么。请求过的 `X-GM-*` 缺了按现有规则算不完整。
- `X-GM-LABELS` 的 STORE 不用 `.SILENT`：Gmail 是否支持没有验证过，非 SILENT 的答复只是多几行 FETCH。
- **假服务器的 Gmail 模式**：邮件只在三个位置里，标签邮箱在每条命令前从"所有邮件"重建（保留各自的 UID）。
  比把同一封邮件在每个邮箱里各存一份、改动时互相同步简单，也更接近 Gmail 本身。

## 方案

- schema（`0001_init.sql`）：`accounts.mode`、`folders.kind` / `listed`、`messages.gm_msgid` / `gm_thrid`
  （部分唯一索引）、`message_labels`（复合外键，冗余 `list_date`，索引 `(folder_id, list_date, message_id)`）、
  视图 `all_mail`、`labels_count_*` 触发器、`messages_flagged`。
- 类型：`FolderKind`、`Folder::kind` / `listed`、`FolderRole::Important`、`AccountMode`、`Account::mode`、
  `GmailLabel`、`FlagState::labels`。
- 存储：`Folders::reconcile`（kind、隐藏标签、kind 变化）、`Folders::label`、`Folders::recount`、
  `Messages::upsert`（gm id、标签）、`update_flags`（标签）、`list`（位置 / 标签 / Starred 三种分页）、
  `Accounts::set_mode`。
- 协议：`Items`、`FetchedMessage::gm_msgid` / `gm_thrid`、`Session::is_gmail` / `uid_store_labels` /
  `uid_search_gm_msgid`，`Command::list`（括号列表参数）。
- 假服务器：`ServerState::gmail`、`FakeMailbox::set_labels`、`X-GM-*` 的 FETCH / STORE / SEARCH、位置之间的
  MOVE、标签邮箱里的 STORE / MOVE / APPEND / `\Deleted`。
- 核心：调用改为 `Items::Standard`、`FolderKind::Location`，同步行为不变。CLI 的角色直接用 `as_str`；GUI 给
  "重要"一个图标和位置（#129 再定样式）。
- 文档：`labels.md`（状态、存储的实现细节、新增"协议"一节、未验证项、迁移步骤）、`storage.md`、
  `protocol.md`、`testing.md`、AGENTS.md。
- 验证：新增 store 测试 `labels_show_and_count_what_is_in_all_mail`（计数每一步都与重新数一遍比对）、
  `label_pages_seek_in_their_index`（查询计划用对索引、没有临时排序）、
  `labels_stay_while_carried_and_kinds_keep_their_messages_apart`（去掉重算时失败）、
  `starred_listed_later_counts_what_is_flagged`、
  `labels_are_found_by_role_or_name`、`mode_is_standard_until_found_otherwise`；协议单元测试（标签解析、
  `\Important`、括号列表编码）；对假 Gmail 的 5 个会话测试（取 id 和标签、加减标签与 MODSEQ、位置之间移动、
  标签邮箱里的操作、没有扩展时报 `Unsupported`）。`cargo test --workspace`、Dovecot 与 submission 真实服务器
  测试通过。没有对真的 Gmail 跑（CI 也没有），同步接上之后（#127）再用测试账户手动验证。
