# 已存的正文按内容版本重建

- 日期：2026-11-03
- 类型：功能

## 背景

正文在下载时解析、清洗后存进 `message_bodies`，之后一直用这份结果。改了清洗规则（修安全问题）或解析逻辑，
已经存下的正文不受影响；#195 的后台预取让大多数正文在打开前很久就存下，问题更常见（#198）。另外读信区的
`recolor_html` / `embed_inline_images` 只接受当前清洗器的原样输出（开头一个 `<style>`），早期格式的正文不满足。

## 决策

- 版本记在 `message_bodies.content_version`，常量是 proto 的 `mime::CONTENT_VERSION`：存下的是整个 `Content`，
  解析和清洗都会改变它，所以不放在 `catmail-html`。规则写在代码里，没法取 hash；改由指纹测试钉住一组邮件的
  输出，一变就失败，强制改的人决定升不升版本。比较用 `<`，新旧构建来回切换不反复重建。
- 有原文的从原文重新解析；没有原文的只把存下的 HTML 再清洗一遍（幂等，收紧的规则生效），文本不动。放开的规则
  对后者不生效：重新下载要占预算、要联网，又都是最大的邮件，接受。
- 打开时按需重建，用户看到的总是当前规则；后台另做一遍，让搜索跟上。后台重建只用本地 CPU 和磁盘，不复用预取
  （预取按账户、等同步、受预算），照 GC 的样子做成引擎级的一次性任务。不做成前端的开关（考虑过 `MailClientBuilder` 上的选项，但没有前端会
  不开，忘了开的前端搜索会一直用旧文本）：引擎总是启动它，延迟在 `EngineTuning::rebuild_delay`，短命的命令行
  在延迟到之前就关掉了。
- 没有原文时的"再清洗、按 `cid:` 判内嵌"与解析时用同一个 proto 函数（`mime::sanitized_html`），不在 core 里
  另写一遍，规则只有一处、指纹测试也覆盖到。
- 附件按 section upsert，保留 id：读信区拿着附件 id，删了重插会让正在显示的邮件点附件失败。
- 存正文（打开、预取、重建）提交后，Gmail 上也通知展示这封邮件的标签和 Starred：一行邮件在那些列表里也显示
  摘要和附件标记，原来只通知所在位置（All Mail），GUI 的标签列表不刷新。这是打开、预取原有的问题，重建照着
  做时被发现（review），一并修在 store（`store_body` / `rebuild_body` / `refresh_html` 记下
  `touched_labels`），调用方发事件。

## 方案

- `0001_init.sql`：`content_version`（默认 0，现有正文都会重建一次）和索引 `message_bodies_version`（找不到
  旧正文时不必读整张正文表）。
- store：`NewBody::version`、`MessageLocation::body_version`、`Messages::stale_bodies` / `stale_body` /
  `rebuild_body` / `refresh_html`（都按旧版本比较后写入，并发时只生效一份；`rebuild_body` 与 `store_body`
  共用 `store_parts`，文本没变时不重写索引），附件写入统一为按 section 的 upsert（`Attachments::store`）和
  `mark_inline`；`BlobStore::put_bytes` 先算 hash，已存在的不再写临时文件。
- proto：`CONTENT_VERSION`、`sanitized_html`，指纹测试 `fingerprint_follows_content_version`。
- core：`rebuild.rs`（`on_open`、后台 `run`，每 20 封一个事务）；`ensure_downloaded` 对旧正文先重建；
  `EngineTuning::rebuild_delay`；`body::stored` 是 `read_blob` 的同步部分，重建也用。
- 文档：`bodies.md` 新增"按版本重建"一节。
- 测试：store 的 `rebuilds_bodies_in_place`，`labels_show_and_count_what_is_in_all_mail` 断言存正文、重建后的
  `touched_labels`；Gmail 的 `storing_a_body_tells_the_labels_showing_the_message`（下载和重建都通知 INBOX、Work、
  Starred，去掉修复后失败）；core `bodies.rs` 的三个（从原文重建、无原文再清洗、后台重建），
  去掉打开时的重建后前两个失败。
