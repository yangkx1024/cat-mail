# 补测试：SMTP STARTTLS 明文注入、带 CR/LF 的邮箱名

- 日期：2026-10-20
- 类型：修复

## 背景

Issue #27：两处协议层代码是对的，但没有测试钉住。

1. SMTP 在 STARTTLS 的 220 之后用 `into_inner()` 丢掉 `BufReader` 里的明文缓冲，再在 TLS 上重新 EHLO。
   假 SMTP 服务器早有 `inject_after_starttls` 故障，但 `implicit_tls_and_starttls` 只断言发信成功，
   没有断言伪造的应答没被采信。
2. 服务器给的邮箱名原样用作 `raw_name`。imap-types 对含 CR/LF 的名字退化成同步 literal，这是安全的
   组帧，但没有测试断言 `examine("Inbox\r\nX")` 发出的是 literal，而不是报错或把 CR/LF 拼进命令行。

## 决策

- 伪造的 EHLO 应答改成宣称 `SIZE 1`，真实应答不设 `size_limit` 就不带 SIZE，于是 `max_size()` 能直接
  看出客户端采信了哪一份（`has()` 是私有的，不为测试加公开 API）。像 IMAP 的同名测试一样，握手失败
  也可以接受，采信明文不行。
- 假 IMAP 服务器按行切命令，只有 literal 能把 CR/LF 带进同一条命令，所以断言 EXAMINE 之后日志里
  恰好一条、名字完整的命令就足以说明发的是 literal。考虑过给 `LoggedCommand` 加 literal 个数，但它
  推得出来，还要让 SMTP 假服务器填一个没意义的字段，放弃了。

## 方案

- `catmail-testkit`：`SmtpFaults::inject_after_starttls` 注入 `SIZE 1`。
- `catmail-proto/tests/smtp_session.rs`：注入的情形从 `implicit_tls_and_starttls` 拆成单独的
  `starttls_ignores_plaintext_injection`，断言 `max_size()` 为 `None`，且之后登录、发信的应答仍然对得上。
- `catmail-proto/tests/imap_session.rs`：`mailbox_names_with_line_breaks_go_as_literals`。
- 验证：临时把 SMTP 改成把剩余的明文缓冲接到 TLS 流前面，新测试失败（`max_size()` 为 `Some(1)`）；
  临时让 `mailbox()` 拒绝含 LF 的名字，IMAP 测试失败。两处都已还原。
