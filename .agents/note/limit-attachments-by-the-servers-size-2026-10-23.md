# 按服务器公布的 SIZE 决定附件上限

- 日期：2026-10-23
- 类型：功能

## 背景

Issue #88：#40 给草稿附件定了固定上限 25 MiB，对所有账户一样。但 SMTP 服务器在 EHLO 里公布自己收多大的
邮件（`SIZE`，RFC 1870），`smtp::Session::max_size` 已经读到，只在发送那一刻用。收得多的服务器
（Fastmail 约 70 MB）被我们自己拦下，收得少的（Postfix 默认 10 MB）要写完点发送才失败。

## 决策

- 按发件身份记录 SMTP `SIZE`（见同日的发件身份），按账户记录
  IMAP 的 `APPENDLIMIT=<n>`（RFC 7889）：Drafts 和 Sent 的副本也要 APPEND。只认能力里的全局值，
  按文件夹的（STATUS 里）不管。`APPENDLIMIT=0` 照 RFC 7889 §5 是"不收 APPEND"：proto 照实返回 0，
  core 记录时不当作大小（副本多小都存不进去，拦附件也没用，发信照常），列里的值始终是"最大邮件"。
- 额度 = min(SIZE, APPENDLIMIT) 各减 64 KiB 余量（头、正文、每个附件的部件头）再按 base64 折算
  （57 字节一行 78 字符）。只是估算，发送时照旧检查真实大小，所以不按正文实际长度精算。
- 不知道时 SMTP 一侧仍用 25 MiB；无论服务器多大都不超过 50 MiB（整封信在内存里构建、每次修改整封复制到
  Drafts）。issue 说"常量仍要作为上限"，但若上限就是 25 MiB，收得多的服务器也用不上，所以拆成默认值和
  上限两个常量。Gmail 的 `SIZE 35882577` 算出 24.96 MiB，比原来的 25 MiB 略少：原来那个值只给头和正文
  留了约 10 KB。
- 每次连接都比较、不同就写（服务器会调高调低，也会不再公布）；先读再写，平时不占写连接。记录失败只写
  日志，不让连接失败。
- 添加账户时的连接测试（现在也登录 SMTP）拿到的值：新增 `add_account_checked` /
  `add_identity_checked`，先检查、再在插入的事务里连同拿到的值一起写进去，前端原来"先 `check_login` 再
  `add_account`"的两步换成这一步。起初用引擎里按（协议、服务器、用户名）记下最近几次检查结果的缓存，
  让之后同样设置的 `add_account` 取走，`/simplify` 时改掉：两个调用之间靠隐藏状态传数据，签名上看不出来。

## 方案

- schema：`accounts.imap_append_limit`、`identities.smtp_max_size`；`Accounts::append_limit`、
  `set_append_limit`，`Identities::set_smtp_max_size`（超过 i64 的存成 i64::MAX），`Drafts::size_limits`
  一次查出草稿的发件服务器和账户的两个值。
- proto：`imap::Capabilities::append_limit`；testkit 的假 IMAP 可公布 `APPENDLIMIT`（`Faults::append_limit`）。
- core：`connection::remember_limit`，在 `connect`（IMAP）和 `connect_smtp` 里与连接前读到的值比较；
  `connection::Sender` 带上发件人的 SIZE；`MailClient::add_account_checked` / `add_identity_checked`；
  `drafts::attachments_limit` 和 `attach_file` 按草稿的发件人取额度，错误信息里报实际的上限。
- 文档：`docs/architecture/sending.md` 的草稿一节、README。
- 测试：`attachments_limit` 单测（Gmail 的值编码后放得下、Fastmail 更多但不过 50 MiB、Postfix 更少、
  APPENDLIMIT 取小）；`tests/send.rs` 发信后学到更小的 SIZE、服务器调高后放宽、不再公布时回到默认，
  同步学到 APPENDLIMIT，`add_account_checked` 之后第一次发信前就生效；`tests/identities.rs` 按草稿所选身份的服务器
  算；store 的读写单测；`Capabilities::append_limit` 单测。
