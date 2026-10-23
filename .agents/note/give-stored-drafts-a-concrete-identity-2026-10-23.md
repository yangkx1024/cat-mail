# 已存的草稿带具体的发件身份

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #90：#80 之后 `drafts.identity_id` 必填，但 `DraftContent::identity` 仍是 `Option<IdentityId>`，既作
输入也作读出。`None` 只在写入时有意义（存成默认身份），读出的草稿总是 `Some`，于是 `connection::sender`、
写信窗口的 `FromField::One`、CLI 的 `JsonDraft.identity`（可为 `null`）都在处理不会出现的 `None`；
`Drafts::update` 收到 `None` 还会把草稿改回默认身份，自己拼一个 `DraftContent` 再更新的调用方可能无意中
换了发件人。

## 决策

- 身份挪出 `DraftContent`，放到 `Draft::identity: IdentityId`。只有新建接受"用默认"：
  `create_draft(account, from: Option<IdentityId>, content)`；`update_draft(id, from: IdentityId, content)`
  必须给出身份。
- 考虑过保留在 `DraftContent` 里、改成必填：新建时就得先查默认身份，或者 `create_draft` 另收一个
  `Option` 和内容里的身份打架；`DraftContent::default()` 也会带出一个无效的 id。
- `Drafts::update` 里"身份已被删除就保留草稿现在的"回退保留：写信窗口在收到 `IdentitiesChanged` 之前
  保存时靠它（删除时草稿已改到默认身份）。
- 回复把按原信选出的身份（`reply_content` 一并返回）直接传给 `insert`，转发用默认身份，和原来一样。
- CLI JSON 的 `identity` 不再为 `null`：只是收窄，不算改变含义，`schema_version` 不变。
- 写信窗口把"上次交给引擎的"存成 `Written { from, content }`，比较、保存都按这一对；下拉框没有选中项时
  沿用已存的身份。

## 方案

- types：`DraftContent` 去掉 `identity`，`Draft` 加 `identity`。
- store：`Drafts::insert(account, identity: Option<_>, ..)`、`Drafts::update(id, identity, ..)`（只查一次身份
  属于哪个账户）；`Identities::sending` 只收具体的身份，`exists` 去掉；测试里的 `edit` 辅助函数保留草稿的身份。
- core：`drafts::create` / `update` / `insert` 带身份参数；`reply_content` 返回 `(DraftContent, Option<IdentityId>)`；
  `connection::sender` 去掉，`draft_sender` 直接用 `Identities::get`（复合外键保证身份属于草稿的账户）；`MailClient::create_draft` / `update_draft` 新签名。
- CLI：`compose`、`reply`、`forward`、`draft edit` 用 `--from` 或草稿已有的身份；`JsonDraft.identity: i64`。
- GUI：`FromField::One(IdentityId)`，`from_field` / `sender_identity` 不再是 `Option`。
- 文档：`docs/architecture/sending.md` 的"草稿选身份"。
- 测试：store 的 identities 测试去掉"不带身份更新就回到默认"那一段（已无法表达），补上删除身份后用旧身份
  保存仍是默认身份；集成测试经 `Env::edit`（保留草稿的身份）更新草稿。fmt、clippy、全部测试通过。
