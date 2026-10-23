# 按地址找身份统一到 core；回复、转发建草稿时定发件身份

- 日期：2026-10-23
- 类型：重构

## 背景

issue #94。"从地址找到账户的身份，地址相同时默认身份优先"写了两份：core 回复选身份（`drafts.rs` 的
`Own` 列表靠顺序）和 CLI `--from`（`resolve_from` 靠 `list_identities` 默认在前的顺序）。CLI 的
`reply --from` / `forward --from` 还要在草稿建好后再 `update_draft` 改身份，多一个草稿版本、多一次上传。

## 决策

- 规则放进 store 的一条查询 `Identities::by_email`（`ORDER BY is_default DESC, id LIMIT 1`，`email`
  列本身 `COLLATE NOCASE`），core 用 `MailClient::identity_by_address` 暴露给前端。没有做成"在身份列表里
  找"的纯函数：那仍要每个调用方保证列表顺序。
- `reply_content` 保持纯函数：`reply` 先在读事务里把原信 From/To/Cc 的每个地址查成身份（账户自己的地址
  没有身份时记为 `None`，即默认），再交给它；`own` 里地址对应的身份已经选定，不再依赖顺序。
- `reply` / `forward` 增加 `from: Option<IdentityId>`。CLI 仍要先读原信拿账户（解析地址需要），但身份在
  建草稿时就定下；只有给了正文或收件人才 `update_draft`。

## 方案

- `catmail-store/src/identities.rs`：`by_email` 及测试（默认优先、其次最早、大小写、不跨账户）。
- `catmail-core/src/drafts.rs`、`client.rs`：`identity_by_address`，`reply` / `forward` 的 `from` 参数；
  GUI 和测试调用处传 `None`。
- `catmail-cli/src/cmd/identity.rs`、`compose.rs`：`resolve_from` 用 `identity_by_address`（数字仍按 id）；
  `reply` / `forward` 把身份传给 core。
- 测试：core `tests/identities.rs` 的 `answers_are_made_as_the_identity_asked_for_or_written_to`，CLI 的
  `answers_are_made_as_the_identity_from_names`。
- 文档：`docs/architecture/sending.md`、`cli.md`。
