# 设置要 TLS、服务器用明文回答时说清楚

- 日期：2026-11-05
- 类型：修复

## 背景

在 nsh 版的登录页重新登录 iCloud，SMTP 填了 `smtp.mail.me.com`、端口 587，安全方式留在默认的 TLS，报
"Couldn't sign in: outgoing mail server (SMTP): connection failed: received corrupt message of type
InvalidContentType"。587 是 STARTTLS 的端口：服务器先用明文发 `220 …`，rustls 把它当 TLS 记录读，第一个字节
不是合法的记录类型。错误是 rustls 的原话，看不出该改什么；登录失败也没写进日志，只能从备份数据库里查出这个
账户原来的设置（587 + STARTTLS）来推断。

复现：`Security::Tls` 连一个用明文打招呼的端口（SMTP 的 587、IMAP 的 143）。

## 决策

- 在 `net::connect` 里认出这种失败：只在"一开始就 TLS"的握手上，且 rustls 的错误是
  `InvalidMessage::InvalidContentType`。STARTTLS 升级时的握手失败另有原因（中间人、服务器坏了），不改。
- 报成 SMTP / IMAP 各自的 `NotTls { port }`，说明该怎么改：选 STARTTLS，或用 TLS 的端口（465 / 993）。
  算设置错误（`ErrorKind::Invalid`），SMTP 的也算永久错误，重试没用。
- 考虑过按端口自动选安全方式（587 就用 STARTTLS），但那会替用户改设置，非标准端口也猜不准，不做。
- 登录检查失败时，core 的 `add_account`、`add_identity` 记一条 warn：两个服务器的主机、端口、安全方式和错误，
  不记地址和用户名。放在 core 而不是前端，CLI 和两个前端都有。

## 方案

- `catmail-proto`：`net.rs` 的 `NetError::NotTls`、`answered_in_plaintext`；`smtp/error.rs`、`imap/error.rs`
  的 `NotTls` 及其分类。
- `catmail-core/src/client.rs`：`add_account`、`add_identity` 登录失败时的日志，`server()` 格式化服务器。
- `docs/architecture/sync.md` 补了一条。
- 验证：`net::tests::tls_to_a_plaintext_port_says_so` 用明文打招呼的本地服务器（SMTP、IMAP 各一种问候），
  去掉映射时它失败，报的正是原来的 `InvalidContentType`。
