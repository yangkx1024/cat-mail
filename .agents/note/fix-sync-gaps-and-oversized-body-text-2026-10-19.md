# 修复：BODYSTRUCTURE 无法解析的邮件不入库；超大正文部分无处可看

- 日期：2026-10-19
- 类型：修复

## 背景

M2 合并后评估遗留问题，修其中两个：

- **（M1 遗留）** 同步拉邮件头时 `execute_lossy` 会丢掉无法解析的整条 FETCH 响应。如果
  原因是 imap-codec 不接受服务器发来的 BODYSTRUCTURE，每次同步都会重现：这封邮件永远不
  入库（看不到、读不到、搜不到），那一批永远不完整，文件夹游标永远不推进，之后每次同步都
  退化为 `UID SEARCH ALL` + 全量求差集，并重拉这封邮件。复现：假服务器对某个 UID 发畸形
  BODYSTRUCTURE（新增故障 `broken_bodystructure`）。
- **（M2 已知限制）** 超过 2 MiB 的邮件按部分下载时，超过 2 MiB 的正文部分（大段日志、
  内嵌 `data:` 图片的 HTML）既不下载也不列为附件，`read` 里看不到，只能 `--raw`。

## 决策

- 有行无法解析（`execute_lossy` 的 `skipped > 0`）时，缺失的 UID 立即用不带 BODYSTRUCTURE
  的 FETCH 再取一次，`has_attachments` 记为否，下载正文后按完整 MIME 树重算。下载路径对坏
  BODYSTRUCTURE 本来就会退回整封下载，不用改。重试放在 `Session::fetch_summaries` 里，
  调用方（sync）不变；只按 `skipped` 触发，SEARCH 之后被删除的邮件不会多一次往返。没有逐封重试或为仍然缺失的邮件存占位行：那时原因已不是
  BODYSTRUCTURE，先不为想象中的情况加复杂度。
- 超大正文部分作为附件列出（`text/plain` / `text/html`，无文件名，非内嵌），复用
  `attachment_content` 按 section 下载。考虑过在正文里插一行提示，放弃：引擎不应向正文
  写界面文案，也会进全文索引；前端可以从附件类型看出来。
- 判断"哪些部分是正文文本"抽成 `body_text`，`text_sections`（要取什么）和
  `content`（取回后列出没取的）共用，保证两者一致。
- /simplify 整理：重试改按 `skipped` 触发；`leaves()` 直接返回按 section 的 map，每次调用
  只建一次；`extract` 把附件与额外列出的 part id 合并进 `BTreeSet`（有序、去重；codex
  review 指出逐个 part 调 `Vec::contains` 是平方复杂度，2 万个附件的邮件要 1 秒，改后
  约 70 ms）；测试去掉多余的辅助闭包。没采纳：
  把这类部分标成"正文"而非附件（避免 `@` 标记，需要新增列，属于设计改动，留待以后）；
  不改 `extract` 签名、在 `content` 里单独构造这些部分（代码更多）；把 `commands_since`
  挪进 testkit（超出本次改动范围）。

## 方案

- `catmail-proto`：`session.rs` 的 `fetch_summaries` 拆出 `fetch_summaries_once`，缺失
  的 UID 不带 BODYSTRUCTURE 重取；`mime/content.rs` 的 `extract` 增加 `also_listed`，按
  part id（MIME 顺序）合并进附件列表；`mime/structure.rs` 新增 `body_text`。
- `catmail-testkit`：新故障 `Faults::broken_bodystructure`（指定 UID 每次都发畸形
  BODYSTRUCTURE）。
- 文档：`docs/architecture.md` 同步容错与正文部分两处、README 使用说明。

验证（新增测试都先在未修复的代码上确认失败）：

- `sync.rs::unparsable_bodystructure_does_not_hide_the_message`：三封全部入库，第二次
  同步不发 SEARCH 和 FETCH（游标已推进），下载后 `has_attachments` 被纠正。
- `imap_session.rs::unparsable_lines_make_authoritative_answers_fail` 改为断言重取：第二条
  FETCH 只含缺失的 UID、不带 BODYSTRUCTURE，不存在的 UID 仍然缺失；没有丢行时缺失的 UID
  不重取。
- `structure.rs::text_too_large_to_fetch_is_offered_as_an_attachment`：alternative 里过大
  的 HTML、mixed 里唯一且过大的正文，按 MIME 顺序列出、估算大小。
- `bodies.rs::text_too_large_to_fetch_can_be_opened_as_an_attachment`：3 MiB 纯文本邮件
  下载正文时不取该部分，作为附件打开得到原文。
