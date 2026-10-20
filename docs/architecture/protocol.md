# IMAP / SMTP 连接

[← 架构](../architecture.md)

SMTP 客户端见[草稿与发信](sending.md#smtp-客户端)。

## IMAP 库

`imap-next`（sans-IO，基于 `imap-codec`），启用 `starttls` 与 `ext_condstore_qresync`，不启用它
自带的 `stream`：`catmail-proto` 自己驱动状态机，底层可以是任意 `AsyncRead + AsyncWrite`，便于
STARTTLS 升级和测试。`imap-codec 2.0.0-alpha.9` 未开启 `nom/alloc`，`catmail-proto` 显式依赖
`nom` 补上（见其 `Cargo.toml` 注释），上游修复后可移除。

## 连接层

- **默认严格。** 无法解析的响应行由连接层计数，`Connection::execute` 只要丢弃过一行就返回
  `Incomplete`：LIST、SEARCH、EXAMINE、flags 这类结果缺一行就会被误当成删除或"没变化"。新命令
  默认就是安全的；只有明确能处理缺行的调用（拉邮件头）用 `execute_lossy`，见
  [同步 / 容错](sync.md#容错)。
- **修补已知的服务器错误。** imap-codec 遇到不合语法的字段会丢掉整条响应，同一封邮件每次同步都会
  丢。连接层在 imap-next 解析之前修补读到的字节（`imap/quirks.rs`）：只改响应行，literal（邮件内容）
  原样通过，按 imap-codec 的 fragmentizer 的规则（行尾 `{n}` / `{n+}`）判断 literal 从哪里开始，两边
  才不会错位。目前只有一项：QQ 邮箱的 `INTERNALDATE "2-Oct-2026 …"` 缺少一位日期前的空格（RFC 3501
  要求 `" 2"` 或 `"02"`），每月 1–9 号的邮件因此全部拉不到。修补层按行缓存，但一行超过 64 KiB 还没结束时
  先把前面的部分交给 imap-next（只留最后 15 字节，够看到一个被截断的修补位置），不然一个不换行的服务器
  能让缓存无限增长，绕过 imap-next 对单个响应 100 MiB 的上限。行是否声明了 literal 不从缓存里看，而是
  逐字节跟踪（与 fragmentizer 同一个状态机）：`{n}` 的长度可以带任意多个前导零，不能指望它留在缓存里。
- **APPEND 被提前拒绝。** 带 literal 的命令发出第一行后，服务器可以不回 `+` 而直接回 tagged NO
  （Dovecot 无法写入时就这样）。imap-next 只在 BAD 时放弃这条命令，收到 NO 后仍等着发送剩下的
  literal，之后排队的命令永远发不出去。所以命令完整发出之前收到 NO/BAD 时返回 `Error::Aborted`
  （致命）：连接不再放回连接池，`Connection` 自己也记下，之后的命令立即失败。
- **一条命令的回答总量有上限。** imap-next 只限制单条响应 100 MiB，不限条数，而一条命令的全部 untagged
  响应要攒到 tagged 完成才返回。所以连接层按字节计数，一条命令读到超过 256 MiB 就返回 `Error::Protocol`
  并把连接标成不可用（剩下的回答不能被当成下一条命令的）。我们要的最大回答是一整封邮件，正常服务器碰不到。
- **错误和日志里不放 imap-next 的事件本身**，只放变体名（`connection.rs` 的 `event_name`）：事件可能
  带着含凭据的命令，而 imap-types 的 `Secret` 只在 release 构建里脱敏。
- 每次网络读写超时 60 秒。
- TCP keepalive：安静 60 秒后开始探测，间隔 15 秒（`net::configure`，经 socket2；tokio 不暴露）。NAT
  无声地忘掉连接时几分钟内就能发现，而不是等到下一条命令或 IDLE 到期。
- 命令装不下的输入（APPEND 的邮件里有 NUL，IMAP literal 不能带）返回 `Error::InvalidInput`：什么也没
  发出，连接照常可用，不是致命错误。

## IDLE

`Session::idle(stop, max)`（RFC 2177）在选中的文件夹上 IDLE，直到服务器报告变化（EXISTS、EXPUNGE、
VANISHED、FETCH）、`stop` 完成或过了 `max`，然后发 DONE、等 tagged OK，返回是否有变化。

- IDLE 期间的读不设 60 秒超时（安静正是目的），由 `max` 兜底；DONE 之后恢复正常超时。等待与读取用
  `select!`：读取可以取消，已读到的字节留在缓冲区里，不会丢。
- 服务器自己结束 IDLE（tagged OK）算正常结束（总是这样时后台同步改为轮询，见
  [后台同步](sync.md#后台同步与-idle)）；回 NO 是 `Error::Command`，后台同步改为轮询。IDLE 期间无法解析
  的行当作"有变化"。
- **顺带报告的变化。** 服务器可以在任何命令的答复里报告选中文件夹的变化，而且只说一次。连接层对每个
  答复记下 EXISTS（数量变了才算）、EXPUNGE、VANISHED，`Session` 打开文件夹时让它从头记起
  （`Connection::mailbox_opened`）：打开以来已有变化时 `idle` 立即返回 true，不发 IDLE。IDLE 以外的
  FETCH 不算（那通常是自己的 FETCH 的答复），别的客户端同时改的 flags 要等下一次同步。同一份 EXISTS
  也让 SEARCH 知道文件夹是否已空。

## 连接池

按需操作（下载正文、附件、APPEND、`replay_pending`）用每账户最多 2 条空闲连接的小连接池；同步用
自己的连接，打开邮件不必等同步结束。

- 空闲超过 5 分钟的连接不再复用。
- 复用的连接已断开时（`Error::is_disconnected`，服务器或 NAT 关掉了空闲连接），换一条**新建**的
  连接重试一次（池里其余的多半也已断开）；新连接上的失败直接返回，被拒绝的命令不会再发一遍。
- 复用的连接仍然每次 EXAMINE：省掉这个往返要依赖服务器绝不在选中期间改变 UIDVALIDITY，而这正是
  UIDVALIDITY 检查要防的情况。
- 归还连接时在连接池的锁内检查账户的"已删除"标记，而删除是先置标记再取这把锁清空连接池，所以不会
  有连接在账户删除后留在池里。

## 认证

`catmail_proto::Credentials` 是协议层看到的全部，IMAP 和 SMTP 共用，目前只有
`Password { username, password }`。IMAP 有 `AUTH=PLAIN` 就用 `AUTHENTICATE PLAIN`，否则 `LOGIN`
（`LOGINDISABLED` 时报错）；SMTP 优先 `AUTH PLAIN`，其次 `AUTH LOGIN`。

密码一路是 `secrecy::SecretString`（core 重新导出）：从 CLI / GUI 读入、`MailClient::add_account`、
`SecretStore`、`Credentials` 到 SASL 响应和 SMTP 的 AUTH 行（`Zeroizing`），`Debug` 输出不含它，释放时
清零。交给 imap-next 的副本和 GUI 输入框里的文本不归我们管，这只是尽力而为：目标是不让密码散落在
日志和随手的格式化里，而不是对抗能读进程内存的人。

凭据从哪来由 `catmail-core/src/auth.rs` 按 `AuthKind` 决定（密码账户读 keyring）。加 OAuth2 只需
新增 `AuthKind` 和 `Credentials` 的变体（XOAUTH2 / OAUTHBEARER）以及刷新 token 的分支，同步和发信
代码不变。

## TLS

`rustls`，crypto provider 用 `ring`（不用默认的 aws-lc-rs，Windows CI 不需要 cmake / NASM）。证书
校验用 `rustls-platform-verifier`，即操作系统的信任库与校验逻辑，企业内部 CA 与浏览器表现一致。
`MailClientBuilder::tls` 可以替换，例如 `TlsConfig::with_roots_only(<DER 证书>)` 只信任自签名证书；
接口只收字节，前端不需要依赖 rustls。

- `tls`：从第一个字节开始 TLS。
- `starttls`：服务器不支持就报错，绝不降级为明文；升级后丢弃明文阶段的一切状态（包括可能被注入的、
  跟在 `OK` 后面的字节）并重新获取 CAPABILITY；PREAUTH 视为不支持。SMTP 同理：丢掉缓冲区里的明文
  字节，重新 EHLO。
- `tls` / `plain` 下的 PREAUTH 问候表示已认证，`login` 直接返回。
- `plain`：明文，只用于本地测试服务器。
