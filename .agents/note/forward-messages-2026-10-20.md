# 转发邮件

- 日期：2026-10-20
- 类型：功能

## 背景

M5 的写信界面要有"转发"，引擎此前只有新邮件和回复。CLI 与 GUI 是平级前端，转发先在 core 和
CLI 里做好。

## 决策

- **`MailClient::forward(message)`**：新草稿，不填收件人；主题加 `Fwd: `（已有 `Fwd:` / `Fw:` /
  `转发：` 前缀的不叠加，与回复的 `Re:` 规则同一写法）；正文是两个空行、
  `---------- Forwarded message ---------`、原信的 From / Date / Subject / To / Cc（有的才写），再接
  原文，不加 `> `。不设 In-Reply-To / References：转发开始的是另一段对话。
- **附件随转发带上**，只取非内嵌的部分：草稿是纯文本，HTML 里的 `cid:` 图片带过去也没有地方显示。
  没下载的正文和附件先下载（大邮件的附件按需下载，走 `attachment_content` 同一路径）。
- **要么全有，要么没有**：先确保全部附件都在 BlobStore 里（没下载的先下载），再在**一个**
  `Store::write` 里插入草稿和所有附件行（与新建、回复共用 `insert`）。考虑过先建草稿、再逐个
  `attach`：中途失败会留下少附件的草稿，用户未必发现。
- **附件引用原邮件已有的 blob**（`body::attachment_blob`），用 `BlobStore::refresh` 重新开始它的 GC
  宽限期（与 `put` 遇到已有内容时相同），不把内容读进内存再 `put` 一遍：最初那样做，转发几个大附件就
  同时在内存里拿着它们，只为写出同样的 blob。
- **CLI `catmail forward <id>`**：选项同 `reply`，另有 `--to` / `--cc` / `--bcc`；地址和正文文件在
  建草稿之前检查，`--send` 没有收件人直接报错，都不留下草稿。

## 方案

- `catmail-core`：`drafts.rs` 的 `forward`、`forward_content`、`forward_subject`，抽出 `with_body`
  （回复与转发共用"需要时先下载正文"）、`prefixed`（`Re:` / `Fwd:` 共用）、`text_of`、`local_date`；
  `body.rs` 的 `attachment_blob`；`client.rs` 的 `MailClient::forward`。
- `catmail-store`：`BlobStore::refresh`。
- `catmail-cli`：`compose.rs` 的 `Forward` / `forward`（`--to` / `--cc` / `--bcc` 与 `compose` 共用
  `Recipients`），`main.rs` 注册命令。
- 文档：`docs/architecture/sending.md`"草稿"，README 用法。

验证：单测 `forward_subjects`、`refreshing_restarts_the_grace_period`、`forward_shows_the_headers_above_the_text_unquoted`；假服务器测试
`a_forward_carries_the_attachments_but_not_the_inline_images`（正文未下载的邮件，草稿的主题、正文块、
只带 PDF、上传到 Drafts 的副本里附件字节与原件一致）和 `a_forward_without_its_attachment_leaves_no_draft`
（大邮件的附件取不到时没有草稿，恢复后转发成功）；CLI 的 `failed_command_leaves_no_draft` 加上转发的
三种失败。
