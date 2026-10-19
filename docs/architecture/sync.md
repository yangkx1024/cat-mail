# 同步

[← 架构](../architecture.md)

`MailClient::sync_account`：一条连接，逐个文件夹 EXAMINE（只读，不改 `\Recent`）。同一账户的
同步与操作回放由每账户的同步锁串行化。代码在 `crates/catmail-core/src/sync.rs`、`engine.rs`。

## 流程

1. **对账文件夹。** `LIST "" "*"`：新增、更新属性、删除服务器上已不存在的文件夹（连同邮件）。
   - RFC 3501 §6.3.8 规定只要用户有 INBOX 就要列出它。LIST 结果里没有 INBOX 时，
     `Session::list` 用 EXAMINE INBOX 问一次：成功说明响应被截断或损坏，返回 `Incomplete`，
     整次同步报错、什么也不删；失败说明确实没有 INBOX，结果照常使用。
   - 角色依次取：名为 INBOX → SPECIAL-USE 属性 → 常见名称（`Sent Items`、`已发送`、`垃圾邮件`
     等）。按名称猜测只用于没有任何文件夹通过 SPECIAL-USE 声明的角色，每个角色只猜一个。
   - `\Noselect` 的层级节点也保存（`selectable = 0`），但不同步。可选中的邮箱变成层级节点时
     （删除有子文件夹的邮箱会清空它并保留名字），按"这一代 UID 结束"处理：删除邮件、清空
     UIDVALIDITY 与游标、离线操作标为 `conflict`，只保留这一行。
2. **回放操作队列**，在同步任何文件夹之前，让同步看到改动之后的服务器（见
   [操作队列](op-queue.md)）。
3. **EXAMINE 并比对 UIDVALIDITY**（服务器支持时带 `CONDSTORE`）。变化或第一次同步时，在同一个
   事务里删除该文件夹的所有邮件、写入新的 UIDVALIDITY、清空游标，把旧代际的 `pending_ops` 标为
   `conflict`，清掉按旧 UID 指向它的草稿远端副本。
4. **新邮件与已删除邮件。**
   - 游标里的 UIDNEXT 与服务器相同、本地条数等于 EXISTS：没有变化，不发 SEARCH。
   - 否则 `UID SEARCH UID <游标>:*` 只取游标之后的 UID；"本地条数 + 新邮件数 = EXISTS" 说明
     没有删除，不必传输大文件夹的完整 UID 列表。
   - 仍对不上（有删除，或没有游标）时才 `UID SEARCH ALL`，与本地 UID 列表求差集。
5. **已有邮件的 flags。** `UID FETCH 1:<本地最大 UID> (FLAGS)`，有 CONDSTORE 时加
   `(CHANGEDSINCE <游标>)`，HIGHESTMODSEQ 没变则整个跳过；没有 CONDSTORE（或 NOMODSEQ）时
   每次全量拉。有未回放的标记操作时，把它们叠加在服务器的 flags 上（`Messages::update_flags`）；
   只有 MODSEQ 变化不算 flags 更新。
6. **拉取新邮件。** 从新到旧，每批 200 封
   `UID FETCH (UID FLAGS INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (…)])`，
   每批一个事务，提交后发 `MessagesChanged`，界面先看到最新的邮件。
7. **写游标。** 文件夹全部完成后才写（UIDNEXT、HIGHESTMODSEQ，取自 EXAMINE 时刻）。某批拿回的
   邮件比请求的少（SEARCH 之后被删，或响应无法解析）时不写，下次完整求差集。

## 游标与断点续传

游标只代表"一次完整的同步"，不随批次推进：从新到旧拉取时 UIDNEXT 没法按批推进，而已经入库的
邮件行本身就是进度。中断后下一次同步重新求差集，只拉缺的那些（`upsert` 遇到已有 UID 只刷新
flags，重复的批次不产生重复行）。HIGHESTMODSEQ 用 EXAMINE 时刻的值，之后的变化 modseq 更大，
下次 `CHANGEDSINCE` 一定能拿到。

## 容错

- 服务器对某个文件夹回 NO/BAD（例如 LIST 之后被删除），或 EXAMINE 缺少 UIDVALIDITY / EXISTS，
  只记在该文件夹的报告里，其余文件夹照常同步。断线、超时、BYE 让整个同步失败，已提交的进度
  保留。
- 重复或乱序的 FETCH 响应按 UID 去重；不带 UID 的主动 FETCH 和不在请求集合里的 UID 忽略；同一封
  邮件的属性分在几条 FETCH 响应里时按 UID 合并（RFC 9051 允许）。
- 无法解析的响应行默认让命令返回 `Incomplete`（见[连接层](protocol.md#连接层)）：LIST、SEARCH、
  EXAMINE、flags 的结果缺一行，就会被当成文件夹被删、邮件被删或 flags 没变。
- 只有拉邮件头用 `execute_lossy`。一条 FETCH 解析失败通常是 imap-codec 不接受服务器的
  BODYSTRUCTURE，每次都会重现，所以缺失的 UID 立即再取一次、不带 BODYSTRUCTURE
  （`has_attachments` 先记为否，下载正文后重算）。仍然缺失的不出现在结果里，那一批算不完整、
  不推进游标。
- 认证失败、服务器缺少设置要求的能力（如 STARTTLS）在 core 里转成 `Error::AuthFailed` /
  `Error::Unsupported`，错误信息带给用户的建议，前端直接显示。

## 文件夹名

不支持 UTF8=ACCEPT 的服务器用修改版 UTF-7 传输非 ASCII 名称（`&XfJT0ZAB-` = `已发送`）。
`folders.remote_name` 原样保存服务器发来的名称，命令里只用它；`name` 是解码后的显示名，解码
失败时等于原名。

## 邮件头

只拉 `HEADER.FIELDS (DATE FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES)`，用
`mail-parser`（`full_encoding`）解析，RFC 2047 编码字和 GBK、GB2312、Big5 等旧字符集都转成
UTF-8。没有（或无法解析）Date 头时用 INTERNALDATE 排序。不合法的 msg-id（mail-parser 会保留
其中的裸 CR，回复时会注入邮件头）在这里就丢掉。`has_attachments` 先由 BODYSTRUCTURE 粗略判断
（`MimeNode::has_attachments`），下载正文后按完整 MIME 树重算。
