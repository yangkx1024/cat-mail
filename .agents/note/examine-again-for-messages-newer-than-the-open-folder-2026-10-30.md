# 打开文件夹之后才到的邮件，重新 EXAMINE 再取正文

- 日期：2026-10-30
- 类型：修复

## 背景

收到 Gmail 新邮件（Xero）的通知后打开主窗口点它，正文下载失败，提示"the message is no longer on the
server"，再点一次才成功。日志里第一次的 FETCH 是成功返回的（`examine=None`，复用了连接池里早先 EXAMINE
过 All Mail 的连接），之后却没有 "message opened"。

原因是 #159 的"同一个文件夹不再 EXAMINE"：这条连接 02:58 打开 All Mail，邮件 03:12 才到；服务器用
EXISTS 告诉这条连接之前，它看到的文件夹里没有这封邮件，Gmail 对它的 `UID FETCH` 回一个空的 OK
（RFC 3501 §6.4.8：不存在的 UID 直接忽略）。`fetch_raw` 得到 `None`，随后的 `UID SEARCH` 时连接已经
赶上，找到了邮件，代码却仍然返回"已不在服务器上"。第二次点时连接已经知道它，于是成功。复现条件：
连接池里的连接只读打开着某个文件夹，此后到达的邮件在这条连接收到 EXISTS 之前被打开（通知点开新邮件
正是这种情况）。

## 决策

- 复用的条件加上"邮件在打开之前就已到达"：UID 小于 EXAMINE 时的 UIDNEXT。这个判断是精确的，不需要
  先发 NOOP（每次都要多一个往返，抵掉 #159 省下的）；新邮件多一次 EXAMINE（约 0.4 s），UIDNEXT 随之
  更新，同一批新邮件之后照常复用。没有 UIDNEXT 的服务器每次 EXAMINE，和 #159 之前一样。
- 另外也加了兜底：复用的文件夹 FETCH 落空时重新 EXAMINE、再取一次，只重开一次。起初没加，理由是会
  掩盖服务器异常；但重开后仍落空照样报错，什么也没掩盖。它让正确性不再依赖 UIDNEXT 判断是否完备，
  也保证过时的视图不会让 SEARCH 落空、进而删掉服务器上还在的邮件；代价只在这条罕见路径上多一次 EXAMINE。
- 服务器还有这封邮件（SEARCH 找得到）却没回内容时，不再说"已不在服务器上"，改为 `imap::Error::Protocol`：
  它是致命错误，连接连同过时的文件夹视图一起丢掉，再试用新连接。附件 section 回 NIL 也走这里，以前同样
  被误报成"已不在服务器上"。
- GUI 打不开邮件时记一条 warn：这次只能靠用户回忆错误信息。

## 方案

- `catmail-proto`：`Session::examined(raw_name, uid)` 只在 `uid` 小于打开时的 UIDNEXT 时返回 UIDVALIDITY。
- `catmail-core/src/body.rs`：`examine` 传入邮件的 UID，总是打开的部分拆成 `reopen`；`on_server` 在复用的
  文件夹 FETCH 落空时 `reopen` 后重试一次，SEARCH 找得到时返回协议错误。
- `catmail-gui/src/reader/mod.rs`：打开失败记 warn。
- `catmail-testkit`：`UID FETCH` 只回这条连接已被告知的邮件，和 Gmail 一样；之前回的是邮箱的实时内容，
  复现不了这个问题。新增故障 `empty_body_fetches`（前 n 次要正文的 UID FETCH 只回 OK）。顺手修了
  `uid_set_contains` 的平方复杂度：它在每次调用里新建"最大 UID"的缓存，对每封邮件都重新扫一遍邮箱；
  改为每条命令建一次的 `uid_set`。
- 测试：`bodies.rs` 新增"打开文件夹后才到的邮件会重新 EXAMINE"（去掉修复时以同样的方式失败）、
  "复用的文件夹落空时重开一次"（去掉重试时失败）和"服务器还有、只是没回的部分不算已删除"；`imap_session.rs` 新增 `examined` 不认打开之后才到的 UID、
  这样的 UID 在旧视图里 FETCH 不到；连接池测试的文件夹各放一封邮件。
- 文档：`protocol.md` 连接池、`bodies.md`、`testing.md`、`logging.md`。
