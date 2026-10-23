# reply / forward 建草稿时就带上正文和收件人

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #110。CLI 的 `reply --body`、`forward --body/--to/--cc/--bcc` 先用 core 的 `reply` / `forward`
建草稿，再 `update_draft` 改内容：多一个草稿版本，后台同步在两次之间跑过的话还会多上传一次到 Drafts。

## 决策

- `MailClient::reply` 多一个 `text: String`，写在引文上方；回复的收件人由原信决定，不接受调用方的。
- `MailClient::forward` 多一个 `Forwarding { to, cc, bcc, text }`（`catmail-types`），转发本来没有收件人，
  给的就是全部收件人。考虑过两个方法共用一个结构，但回复带收件人时是替换还是追加说不清，放弃。
- 转发的地址在下载原信正文和附件之前检查，错的地址不会白白下载。
- 把文字放在上方的逻辑（`write_above`）从 CLI 移到 core。

## 方案

- `crates/catmail-core/src/drafts.rs`：`reply` / `forward` 在 `insert` 的同一个写事务前就把内容拼好。
- `crates/catmail-cli/src/cmd/compose.rs`：不再调用 `update_draft`；GUI 和测试的调用方传空内容。
- 测试：`tests/drafts.rs::answers_are_made_with_the_users_text_and_recipients`（文字、收件人、错地址不下载
  也不留草稿）；CLI 的 `answers_are_made_as_the_identity_from_names` 检查正文和收件人进了草稿。
- 文档：`docs/architecture/sending.md`、`docs/architecture/cli.md`。
