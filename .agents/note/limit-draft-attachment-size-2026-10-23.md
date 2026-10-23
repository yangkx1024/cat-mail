# 限制草稿附件的大小

- 日期：2026-10-23
- 类型：修复

## 背景

Issue #40：`attach_file` 只检查路径是不是文件，就把整个文件流式复制进 BlobStore。GUI（文件对话框、
拖放）和 CLI（`--attach`）都直接调它，拖一个几 GB 的文件会整个复制进 `blobs/`，之后构建 MIME 还要把它
读进内存，SMTP 服务器也必然拒绝。

## 决策

- 在 core 里限制，两个前端自然都得到；超出报 `Error::InvalidInput`，前端已有的错误提示直接显示原因。
- 限制的是一封草稿所有附件的合计，而不是单个文件：很多个小文件同样会让邮件过大。
- 上限用常量 25 MiB：Gmail、Outlook 和大多数服务器的附件上限。按 base64 编码（每 57 字节一行 78 字符）
  约 35,872,336 字节，正好在 Gmail 公布的 `SIZE 35882577` 以内。最初定的 50 MiB 大多数服务器都会拒收。
  按账户记录 SMTP `SIZE` 后再据此调整，留到以后（#88）。
- 转发复制原信附件不受限制（服务器已经收过这封信），但计入之后添加的附件。

## 方案

- `catmail-store` `Drafts::attachments_size`：草稿附件的合计大小。
- `catmail-core/src/drafts.rs` `attach_file`：复制前按打开的文件的大小和剩余额度先拒一次，避免复制；
  复制用 `take(额度 + 1)`，文件读的时候变大或报的大小不对也不会多复制；写入事务里再按存下的大小查一次，
  防止并发添加超出。被拒的 blob 没有引用，留给 GC。
- `docs/architecture/sending.md` 记下这个限制。
- 测试 `tests/send.rs` `attachments_are_limited_in_size`：用稀疏文件，超大的文件被拒且 BlobStore 没变大；
  合计正好到上限可以，多一个字节被拒；删掉一个附件后又能添加。没有修复时这个测试失败。
