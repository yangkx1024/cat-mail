# 同步时用 IMAP PREVIEW 填摘要

- 日期：2026-11-16
- 类型：功能

## 背景

#123。摘要只在取到正文后本地算，没预取、也没打开过的邮件没有摘要，CLI 和 MCP 列出的邮件里这一项是空的。
支持 PREVIEW（RFC 8970）的服务器（Dovecot、Cyrus / Fastmail、Stalwart）可以在同步时给出摘要，不下载正文。
#294（PR #296）已让本地摘要以 preheader 开头，和服务器的说法一致，这是前提。

## 决策

- **先用 Docker 里的 Dovecot 2.4.1 实测**，结果改变了 issue 里的设想：`PREVIEW (LAZY)` 对没生成过摘要的
  邮件一律回 NIL，不带 LAZY 取过一次后才有缓存。所以光随邮件头懒取几乎拿不到东西，必须再补取。另外：
  非 ASCII 的摘要以 literal 返回；没有正文回 `""`；只有引用时回 `>only quoted text`；链接地址
  （`<https://…>`）和 `&zwnj;&#847;` 填充原样保留。
- **两步取。** 拉取新邮件的 FETCH 带 `PREVIEW (LAZY)`，手头有就顺带拿到（预先生成的服务器）；文件夹同步
  最后再取一批（`fetch_batch`，从新到旧）还没有摘要、没下载正文的，不带 LAZY。每次同步一批，大文件夹几次
  同步后补齐，不让一次同步卡在服务器生成摘要上。放在写游标之后，NO 只记日志，下次再试。
- **`messages.previewed` 记"服务器答过了"**，含答空串、以及回答里缺了的（被删或解析不了），避免每次同步
  都问同一封。考虑过用 `snippet = ''` 表示"没有"，省一列，但读路径要特殊处理空串，含义也不清楚。候选查询走
  部分索引 `messages_preview`（`folder_id, list_date`，只含 `body_state = 'headers' AND previewed = 0`）；
  不支持 PREVIEW 的服务器上这个索引不会被读，代价是每行一条索引项。
- **服务器的摘要和本地的一样清理**：`mime::preview_snippet` 就是 `snippet(preview, None)`，去引用行、链接地址、
  不占位置的字符，截到 200 字符。Dovecot 的那段营销邮件摘要清理后与本地摘要逐字相同（单元测试）。
- **本地的优先**：`store_previews` 只写 `body_state = 'headers'` 的行，`store_body` 照常覆盖 `snippet`。
- 重试拉取新邮件（BODYSTRUCTURE 解析失败时）不带 PREVIEW：它的字符串不是 UTF-8 时同样会让那封解析不了。
- 清理放在 proto：它本来就给出清理好的本地摘要和解码好的邮件头，服务器的摘要也一样交出去，core 只管存。
- GUI 列表仍不显示摘要（另定）。
- /simplify 时没采纳的效率意见：预取开着时跳过补取（预取很快就会下载同一批正文、覆盖摘要）——是策略选择，
  预取关着或超限时补取正是用处所在，留待以后看；新邮件不带 LAZY 直接要（省一次往返）——离线久了新邮件
  成批时会让拉邮件头等服务器生成摘要；不支持 PREVIEW 的服务器上插入时就记 `previewed`（省索引写入）——
  改变了列的含义，服务器后来支持了也补不上。

## 方案

- proto：`FetchedMessage::preview`（`Option<Option<String>>`：没给 / 给了但没有 / 摘要，已清理）；
  `Session::supports_preview`、`fetch_previews`（返回按 UID 的清理后摘要）；`merge_fetches` 认
  `AttributeValue::Preview`（项目锁定的 imap-proto 已支持）；`mime::preview_snippet`。`fetch_summaries_once`
  的 `structure` 改名 `optional`：第一次带上解析器可能拒绝、邮件没有也能存的项（BODYSTRUCTURE、PREVIEW），
  重试时都不带。
- store：`0001_init.sql` 加 `previewed` 列和 `messages_preview` 索引；`NewMessage` 加 `previewed`、`snippet`；
  `Messages::unpreviewed`、`store_previews`（带 UIDVALIDITY 校验，Gmail 上照常记下显示它的标签）。
- core：`sync::new_message` 存随邮件头来的摘要；`sync_folder` 最后调 `fill_previews`。
- testkit：`Faults::preview: Option<Previews>`（`OnDemand`：LAZY 回 NIL，像 Dovecot；`AtHand`；`Refused`），
  `FakeMailbox::append_text`（`append_simple` 换成指定正文）。
- 测试：`content.rs` 的 `preview_snippet_says_what_the_local_one_does`；store 的
  `previews_fill_snippets_until_the_body_comes`；`tests/sync.rs` 四个（分批补齐且不重复问、随邮件头拿到、
  不支持时不问、被拒不影响同步）；`tests/dovecot.rs` 检查真 Dovecot 给的摘要（本机没有 nix，靠 Linux CI 跑）。
- 文档：`sync.md` 新增"服务器的摘要"，`bodies.md`、`protocol.md` 相应处。
