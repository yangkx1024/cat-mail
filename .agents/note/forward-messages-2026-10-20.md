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
- **要么全有，要么没有**：先下载全部附件、`put` 进 BlobStore，再在**一个** `Store::write` 里插入草稿和
  所有附件行。考虑过先建草稿、再逐个 `attach`：中途失败会留下少附件的草稿，用户未必发现。`put`
  紧挨着写事务，GC 的宽限期覆盖其间（与 `attach_file` 相同）；附件内容在内存里拿着，因为
  `attachment_content` 本来就返回整块数据。
- **CLI `catmail forward <id>`**：选项同 `reply`，另有 `--to` / `--cc` / `--bcc`；地址和正文文件在
  建草稿之前检查，`--send` 没有收件人直接报错，都不留下草稿。

## 方案

- `catmail-core`：`drafts.rs` 的 `forward`、`forward_content`、`forward_subject`，抽出 `with_body`
  （回复与转发共用"需要时先下载正文"）；`client.rs` 的 `MailClient::forward`。
- `catmail-cli`：`compose.rs` 的 `Forward` / `forward`，`main.rs` 注册命令。
- 文档：`docs/architecture/sending.md`"草稿"，README 用法。

验证：单测 `forward_subjects`、`forward_shows_the_headers_above_the_text_unquoted`；假服务器测试
`a_forward_carries_the_attachments_but_not_the_inline_images`（正文未下载的邮件，草稿的主题、正文块、
只带 PDF、上传到 Drafts 的副本里附件字节与原件一致）和 `a_forward_without_its_attachment_leaves_no_draft`
（大邮件的附件取不到时没有草稿，恢复后转发成功）；CLI 的 `failed_command_leaves_no_draft` 加上转发的
三种失败。
