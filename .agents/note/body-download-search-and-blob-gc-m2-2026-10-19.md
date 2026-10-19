# M2：正文与附件按需下载、中文全文搜索、blob GC

- 日期：2026-10-19
- 类型：功能

## 背景

M1 只同步邮件头，不能看正文。M2 要能按需下载正文和附件、对中文做全文搜索、回收不再
引用的 blob，并在 CLI 提供 `catmail read` / `catmail search`。README 里 M2 还包括最小
读信窗口，经用户确认拆到后续 PR：它要引入 gpui-kit、flake 里的图形库和更重的三平台 CI，
与引擎部分正交，单独评审更清楚。

## 决策

- **按大小分两条下载路径**（用户选择）：≤ 2 MiB 整封 `BODY.PEEK[]`，保留原文、附件随即
  入库；更大的取 BODYSTRUCTURE + 正文部分，附件第一次打开时按 section 下载。考虑过
  "总是按部分"（最省流量，但完全依赖服务器 BODYSTRUCTURE 的正确性，也拿不到原文）和
  "总是整封"（打开一封带 20 MB 附件的邮件就要下载 20 MB）。BODYSTRUCTURE 解析失败时
  退回整封下载。
- **按部分下载时重建替身邮件**交给 mail-parser 解析，而不是自己实现一套正文选择和解码：
  两条路径的正文选择、字符集、传输编码、RFC 2047/2231 文件名处理完全一致（有测试比对
  两条路径的结果相等）。替身邮件头里的值去掉换行、token 只留合法字符，防止服务器伪造头。
- **"内嵌"按 HTML 是否 `cid:` 引用判断**：mail-parser 把 multipart/related 里的图片归为
  普通附件，而它的 InlineBinary 又会把 mixed 里没被引用的图片当成内嵌。以引用为准，
  `has_attachments` 只计非内嵌部分。
- **写入只检查邮件行还在**：message id 不复用、UIDVALIDITY 变化删除旧行，所以不需要在
  写入时再比对 UIDVALIDITY；用 UID 之前则一定先 EXAMINE 比对。只用 EXAMINE 和 `.PEEK`，
  阅读不会标为已读。
- **按需下载用每账户最多 2 条空闲连接的连接池**，复用的连接已断开时换新连接重试一次。
  同步保留自己的连接，打开邮件不用等同步。完整的每账户 actor 仍留到 M4。
- **全文搜索在 Rust 侧切分 CJK**：相邻双字 + 单字，双字位置连续，多字查询用双字短语
  恰好匹配子串。放弃了自定义 FTS5 tokenizer（需要 unsafe，workspace 禁止）和 trigram
  （不支持两个字的词）。索引由 `Messages` repository 维护（插入时写邮件头，正文下载后
  重写），删除仍由触发器负责。全角 ASCII 折叠成半角，不引入 Unicode 规范化依赖。
- **HTML 用 ammonia 清洗**后存库，保留表格布局属性和行内 style，图片地址原样保留，是否
  加载远程图片交给渲染端。
- **blob GC** 拆成"找候选"和"写锁下重查 mtime 再删"，`put` 持有读锁，消除"GC 判定为旧"
  与"`put` 刷新"交错的窗口；宽限期 1 小时，由 `MailClient::collect_garbage` / `catmail gc`
  触发，暂不自动运行。
- **review 修复**（独立 review 的 6 条意见，均先写出不修就失败的测试）：
  - BODYSTRUCTURE 里类型为 `MULTIPART` 的叶子带着服务器给的 boundary 写进替身邮件，会
    吞掉后面的部分、打乱 section，对不上的部分还被当成"已下载的空内容"存下，之后永远
    返回 0 字节。现在叶子不按 multipart 写、去掉 boundary，对不上的部分视为未下载。
  - `data:` 链接过滤可以用 `&#1;data:`、`da&#9;ta:` 绕过（ammonia 用的 URL 解析器会去掉
    这些字符）；现在按同样规则判断。
  - 行内 `style` 里的 `url(…)` 能绕过远程图片拦截；可能加载资源的 style 整个去掉。
  - 搜索输入里的 NUL 让 FTS5 报语法错误；控制字符按空白处理。
  - 删除账户与下载完成同时发生时，连接可能在清空连接池之后被放回；现在在池的锁内检查
    取消信号。（时序竞争，没有确定性测试。）
  - 保存附件时文件名保留了 bidi 覆盖字符（`invoice\u{202e}fdp.exe`），Windows 保留名
    不全；补齐，并修掉自己在补保留名时引入的按字节切片可能 panic 的问题。
- **codex review 修复**（4 条，均先用测试复现）：
  - 大邮件取正文部分时，服务器回 NIL 或漏掉某个 section 也会得到"成功"的结果，缺正文的
    内容被存为已下载、永不重取。现在要求所有请求的 section 都在，否则退回整封下载。
  - 内嵌判断用子串匹配，`cid:logo-long` 会让 Content-ID 为 `logo` 的附件也被当成内嵌而
    隐藏（`read` 不列出、`--save` 不保存）。现在取出完整的 `cid:` 引用值精确比较，并按
    RFC 2392 忽略 scheme 大小写、做百分号解码。
  - 连接池里两条空闲连接都断开时，重试取到的是第二条断开的连接，下载失败。重试改为总是
    新建连接。
  - 摘要在一个词恰好到第 200 个字符时，分隔空格让计数越过上限，之后整篇正文都进了
    摘要。现在加分隔符前检查是否还放得下下一个字符。
  - 假服务器为此新增：正文 FETCH 闸门、section 回 NIL、关闭当前所有连接。
- **/simplify 整理**（复用、简化、效率、层次四个角度的 review）：
  - 每账户的同步锁、取消信号、空闲连接合并为一个 `AccountState`，删除后保留为墓碑；
    删除流程从三个协调调用变成"开始删除 / 撤销"，按需操作不再需要两次查找。
  - 三个按需入口共用 `on_server`（连接、EXAMINE 与 UIDVALIDITY 检查、取消）和
    `store_blob`；`with_session` 改为直线代码；`raw_message` 对未下载的大邮件只整封取一次。
  - 要取哪些正文部分改由替身邮件 + mail-parser 判定，与显示用同一规则；同步时的
    `has_attachments` 也改走 `MimeNode`，只剩一个 BODYSTRUCTURE 转换；FETCH 按 UID 分组
    合并为 `group_fetches`。
  - 全文索引只剩一个写入入口 `Messages::reindex`；没有 CJK 的文本不再重建字符串。
    `BlobHash` 实现 `FromStr`，去掉两个专用列转换函数；`blob_references` 用 `UNION ALL`，
    并加测试枚举所有 `*_blob_hash` 列。
  - 传输解码直接调用 mail-parser 的宽松解码器，不再拼假邮件；二进制附件不再二次解码；
    取回的 IMAP 数据移动而非复制。`data:` 判断改用 ammonia 同款的 `url` 解析器。
  - `safe_filename` 移到 core（GUI 保存附件也要用）；测试的公共环境移到
    `tests/common`；假服务器复用 `quote`/`literal`，合并两段相同的故障代码。
  - 没采纳：复用连接时跳过 EXAMINE（要信任服务器不在选中期间改 UIDVALIDITY，正是我们
    要防的）；小邮件原文与附件重复存储、附件流式 API、用服务器的 `.MIME` 头代替替身邮件、
    CSS 属性白名单、从 DOM 收集 cid 引用——都是设计层面的改动，留待以后。
- 模式未发布，直接修改 `0001_init.sql`：`attachments` 加 `transfer_encoding`、
  `UNIQUE (message_id, part_section)`，id 改为 AUTOINCREMENT（会交给前端）。

## 方案

- `catmail-types`：`Message`、`MessageBody`、`Attachment`、`AttachmentId`、`SearchQuery`、
  `GcReport`。
- `catmail-store`：`fts.rs`（切分与查询构造）、`attachments.rs`；`Messages` 新增
  `store_body`、`get`、`location`、`search`、`set_raw`；`Db::blob_references`；
  `BlobStore::collect_garbage`。
- `catmail-proto`：`mime/content.rs`（提取正文、附件、section 编号、snippet、传输解码）、
  `mime/structure.rs`（BODYSTRUCTURE 的中立表示与替身邮件）、`mime/html.rs`（清洗）；
  `imap/body.rs`；`Session::fetch_raw` / `fetch_structure` / `fetch_sections`。
- `catmail-core`：`body.rs`（下载、附件、原文）、`connection.rs`（登录与连接池重试，sync
  也改用它）；`MailClient` 新增 `get_message`、`download_message`、`attachment_content`、
  `raw_message`、`search`、`collect_garbage`；新错误 `MessageNotFound`、
  `AttachmentNotFound`、`OutOfSync`。
- `catmail-cli`：`read`（`--html`、`--raw`、`--save`、`--offline`）、`attachment`、
  `search`、`gc`；输出辅助函数移到 `output.rs`，`files.rs` 生成跨平台安全的文件名且
  不覆盖已有文件。
- `catmail-testkit`：BODYSTRUCTURE 与 section 由原文推导，支持 `BODY.PEEK[]` /
  `BODY.PEEK[<section>]`，新增拉正文时断线、挂起两种故障。
- `testenv/fixtures/06-html-inline.eml`：HTML + 内嵌图片 + GBK 正文 + RFC 2231 GBK 文件名。

验证：`cargo fmt`、`clippy -D warnings`、全部测试通过。新增测试：store 8 个（FTS 切分与
真实 FTS5 查询、正文与附件存储、搜索过滤、GC）、proto 15 个（提取、替身邮件与整封解析
结果一致、恶意 BODYSTRUCTURE、清洗及绕过、会话级取正文且不发 `BODY[`）、core 11 个（整封、
按部分、BODYSTRUCTURE 无法解析或正文部分缺失时回退、复用连接已断开（含池中全部断开）、下载失败后重试、旧 UIDVALIDITY 和
已删除邮件被拒绝、并发下载、删除账户取消下载、GC 回收已删除账户的 blob）、CLI 4 个、
testkit 1 个；Dovecot 2.4.5 集成测试扩展到下载各个 fixture、HTML/QP/GBK、按部分下载 3 MB
附件（RFC 2231 文件名）和搜索，并重复运行通过。对 UIDVALIDITY 检查、连接重试、
BODYSTRUCTURE 回退、GC 重查、`store_body` 只写一次、替身邮件头去换行、下载取消以及各项
review 修复做了变异测试，确认对应测试会失败。用 CLI 对 Dovecot 手动走了一遍 sync → read（文本、HTML、保存
附件、原文、离线）→ search → gc，阅读后邮件仍是未读。
