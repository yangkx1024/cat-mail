# 回复和转发缺的原信部分合并下载

- 日期：2026-11-16
- 类型：重构

## 背景

#311。大邮件按部分下载时，附件和内嵌图片不随正文下载。回复、转发要用到时，原来每个部分各发一条 `UID FETCH`：
一封有 20 张内嵌图片的 newsletter 就是 20 次往返。用 CLI 或 agent 回复、转发一封没读过的大邮件时最明显。另外，
MCP 预扣下载额度的规则（`reply_cost` / `forward_cost`）是另写的一份，可能和实际下载的对不上。

## 决策

- 合并 section，而不是取整封原文再在本地拆。取整封要把原文按 BODYSTRUCTURE 对回已存的附件行，回复还会白取用不上
  的大附件。合并 section 一样只要一次往返。整封原文只给作为附件转发用（#312）。
- 每批按服务器给的大小最多 16 MiB：取回的部分存下之前都在内存里。超过的部分单独一批。
- 部分失败：
  - 某个 section 回 NIL：只有它失败。
  - 整条 FETCH 被拒（NO）或回复读不出：改为逐个取，一个坏部分不连累别的。
  - 连不上服务器、邮件已不在服务器上：整个调用失败，但前面几批已存下的留着。

  调用方保留原来的规则：转发的附件有一个取不到就不建草稿，引文的图片取不到只是少一张。
- 转发仍是两次 FETCH：先取附件，再在剩下的额度里取引文的图片。图片要等附件占了多少定下来才能选。
- 下载多少由引擎给（`MailClient::answer_download_size`，按 `Answer`），MCP 用它预扣，也用它算失败后退还多少。
  规则只剩一份，也覆盖作为附件转发。

## 方案

- `body::attachment_blobs`、`batches`、`fetch_batch`；`drafts::forward` 和 `quoted_images` 改用它。引文图片先按
  服务器给的大小选出放得下的，一次取下，再按实际大小核对。
- `drafts::download_size`；`catmail-types` 的 `Answer`、`Forwarding::answer`；MCP 删掉 `reply_cost` /
  `forward_cost`。
- testkit：假 IMAP 的 `nil_parts`（只对指定的 section 回 NIL）、`refuse_several_sections`（拒绝要多个 section 的
  FETCH）。
- 测试：
  - `the_parts_an_answer_needs_are_fetched_together`：回复的两张图片一条 FETCH；某张回 NIL 时另一张照常；转发缺
    一个附件时不建草稿，但已取到的留下；之后只补缺的部分，全部在本地后不再 FETCH。
  - `parts_refused_together_are_fetched_one_at_a_time`：整条 FETCH 被拒时逐个取。
