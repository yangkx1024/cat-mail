# IMAP / SMTP 连接

[← 架构](../architecture.md)

SMTP 客户端见[草稿与发信](sending.md#smtp-客户端)。

## IMAP 库

[`imap-proto`](https://github.com/djc/imap-proto) 只解析服务器的响应；命令编码（`imap/command.rs`）、
分帧（`imap/framing.rs`）和连接的驱动（tag、literal 与续行、AUTHENTICATE、STARTTLS、IDLE / DONE，
`imap/connection.rs`）由 `catmail-proto` 自己写，底层可以是任意 `AsyncRead + AsyncWrite`，便于 STARTTLS
升级和测试。

**暂时依赖 imap-proto 的 main。** 0.17.0 不还原带引号字符串里的转义（`"\\Inbox"` 解析出来是
`\\Inbox`），而解析出来的值看不出原来是带引号的字符串还是 literal，在我们这边没法可靠地补。修复
（djc/imap-proto#209）在未发版的 0.18.0 里，所以工作区用 `[patch.crates-io]` 固定到 main 的一个 commit。
`cargo deny` 只接受 crates.io 和 `deny.toml` 的 `allow-git` 里列出的 git 仓库，imap-proto 的仓库列在那里；
0.18.0 发版、换成 crates.io 的版本时一起删掉（#130）。

为什么不用原来的 imap-next / imap-codec：

- **imap-codec 难以扩展。** imap-types 的命令和响应都是封闭的 enum：`Store` 只能带 flags，FETCH 少一个
  数据项就发不出命令，回答里多一个不认识的项就丢掉整行。每个它不支持的扩展（Gmail 的 `X-GM-*`，以后的
  OBJECTID、PREVIEW 等）都只能等上游，而上游一直是 alpha，每次升级都可能不兼容。
- **imap-next 的状态机要一再绕过：** APPEND 收到 NO 后仍等着发 literal、只限单条响应的大小、事件里带着
  只在 release 构建脱敏的凭据，以及 imap-codec 漏开的 `nom/alloc`。它负责的连接部分我们大多已经自己写了。
- **imap-proto** 只解析响应，正好是客户端需要的部分：语法里最复杂、安全上最敏感的（BODYSTRUCTURE、
  ENVELOPE、literal）用它的，最常需要扩展的（发什么命令）由我们控制。它已经支持 `X-GM-LABELS` /
  `X-GM-MSGID` / `X-GM-THRID`、`EMAILID` / `THREADID`（RFC 8474）、VANISHED 和 MODSEQ；FETCH 里不认识的
  项返回 `AttributeValue::Unknown`，不认识的响应码当作文字（连接层按名字记下，如 `[NOMODSEQ]`），都不会让
  整条响应解析失败。它也是 async-imap（Delta Chat）的解析器，见过大量真实服务器。缺少的响应语法给上游提 PR。
- 不用 async-imap：它是持有连接的会话层，又会回到在别人的状态机外面打补丁。不自己写响应解析器：IMAP
  响应的语法很大，维护和安全上的负担都不值得。
- 试用见[位置与标签 / 验证结果](labels.md#验证结果)：真实 Gmail 的全部答复都能解析。
- imap-proto 把 BODYSTRUCTURE 里的字符串按 UTF-8 有损转换（mail-parser 解析邮件头也一样）。用原始 8 位字节
  写的附件文件名因此另取各部分的 `BODY.PEEK[<section>.MIME]` 重新读，见[正文与附件](bodies.md#原始字节的邮件头)。

## 连接层

- **分帧。** 读到的字节按行和 literal 切成完整的响应（行尾 `{n}` / `{n+}` 声明 literal，长度逐字节跟踪，
  可以带任意多个前导零），literal（邮件内容）原样通过。一条响应超过 100 MiB（声明了还没读到的 literal
  也算）就放弃这条连接。只有 LF 的行尾补上 CR：有的服务器偶尔这样发，而解析器只认 CRLF。
- **默认严格。** 无法解析的响应由连接层计数，`Connection::execute` 只要丢弃过一条就返回
  `Incomplete`：LIST、SEARCH、EXAMINE、flags 这类结果缺一行就会被误当成删除或"没变化"。新命令
  默认就是安全的；只有明确能处理缺行的调用（拉邮件头）用 `execute_lossy`，见
  [同步 / 容错](sync.md#容错)。
- **请求过的 FETCH 项必须都在。** imap-proto 把格式坏掉的已知项（`FLAGS NIL`、只有类型的
  BODYSTRUCTURE）也读成 `AttributeValue::Unknown`，整行照样解析成功。所以 `Session` 检查每封有答复的
  邮件是否带着请求过的每一项，缺了就是 `Incomplete`（flags、大小、整封原文、BODYSTRUCTURE、各 section；
  NIL 的 section 算在）；拉邮件头时缺了 BODYSTRUCTURE 的邮件不带它再取一次。服务器主动多给的、不认识的
  扩展项照样忽略。
- **只有 IMAP4rev2 的服务器。** imap-proto 的 CAPABILITY 解析要求列表里有 IMAP4rev1，只声明 IMAP4rev2
  （RFC 9051）的服务器整条 CAPABILITY 都解析不了。`* CAPABILITY …` 和响应码 `[CAPABILITY …]` 解析失败时
  由连接层自己按空格读出能力列表。
- **宽松地读已知的服务器错误。** QQ 邮箱的 `INTERNALDATE "2-Oct-2026 …"` 缺少一位日期前的空格（RFC 3501
  要求 `" 2"` 或 `"02"`）。imap-proto 不校验 INTERNALDATE 的写法，日期由我们自己解析（`internal_date`），
  三种写法都接受，否则每月 1–9 号的邮件全部拉不到。
- **命令的参数**按字节选最简单的形式：atom（不含 `]`，有的服务器不认），否则带引号的字符串（7 位、没有
  CR / LF），否则 literal；literal 里不能有 NUL（返回 `Error::InvalidInput`，什么也没发出）。服务器有
  `LITERAL+` 时 literal 一律不等 `+`（`{n+}`），有 `LITERAL-`（如 Gmail）时 4096 字节以内不等，否则都等。
- **APPEND 被提前拒绝。** 带 literal 的命令发出第一行后，服务器可以不回 `+` 而直接回 tagged NO
  （Dovecot 无法写入时就这样）。这条命令就此结束，剩下的不再发送，是普通的拒绝（`Error::Command`），
  连接照常可用。
- **一条命令的回答总量有上限。** 单条响应有上限，条数没有，而一条命令的全部 untagged 响应要攒到 tagged
  完成才返回。所以连接层按字节计数，一条命令读到超过 256 MiB 就返回 `Error::Protocol` 并把连接标成不可用
  （剩下的回答不能被当成下一条命令的）。我们要的最大回答是一整封邮件，正常服务器碰不到。IDLE 不攒响应，
  不计数。
- **命令不进错误和日志**，只放命令名（`Command::name`）：LOGIN 带着密码。命令的字节在发出后清零。
- 每次网络读写超时 60 秒。
- TCP keepalive：安静 60 秒后开始探测，间隔 15 秒（`net::configure`，经 socket2；tokio 不暴露）。NAT
  无声地忘掉连接时几分钟内就能发现，而不是等到下一条命令或 IDLE 到期。
- 命令装不下的输入（APPEND 的邮件里有 NUL，IMAP literal 不能带）返回 `Error::InvalidInput`：什么也没
  发出，连接照常可用，不是致命错误。
- **Gmail 的扩展**（`X-GM-EXT-1`）：FETCH 的 `Items::Gmail`、`uid_store_labels`、`uid_search_gm_msgid`，
  见[位置与标签 / 协议](labels.md#协议)。

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
- **flags 变了的邮件。** IDLE 期间的 FETCH 记下序号，随后的 EXPUNGE 让它们跟着前移（被删的那封去掉）。
  序号下一个 EXPUNGE 就可能变，而 IDLE 之后的下一条命令就可能带来 EXPUNGE，所以 `Session::idle` 在
  IDLE 结束时立即用 `FETCH <序号> (UID)` 换成 UID：按序号的 FETCH 期间服务器不许发 EXPUNGE
  （RFC 3501 §7.4.1），换出来的一定对。结果是 `Session::changed_flags`（最近一次 IDLE 的报告，打开文件夹时
  清空）。说不清是哪几封时为 `ChangedFlags::Unknown`：IDLE 期间有无法解析的行、超过一千封（不再逐个记）、
  有序号对不上的 VANISHED，或换 UID 的 FETCH 被拒绝。有 CONDSTORE 时不换、也是 `Unknown`：同步按 MODSEQ
  找出变化，用不上这些 UID，省一次往返。后台同步据此只拉这几封的 flags（见[后台同步](sync.md#后台同步与-idle)）。

## 连接池

按需操作（下载正文、附件、APPEND、`replay_pending`、打开文件夹时的同步）用每账户最多 2 条空闲连接的
小连接池；同步全部文件夹和后台同步用自己的连接，打开邮件不必等同步结束。代码在
`catmail-core/src/pool.rs`：池子本身（每账户一个 `Pool`，挂在引擎的账户状态上）、keepalive，以及复用一条
连接的规则（`with_session`、`on_session`，后台同步自己的连接也走 `on_session`）。"睡前建立的不用"
（`usable`）只写一处，`on_session` 和 keepalive 共用；"只有致命错误才不放回池里"（`survives`）也只写一处，
`with_session` 和 keepalive 共用。后台同步不放回池里，它的连接出任何错都换掉，是它自己的规则。

- 池里的连接一直保持可用，不设存活时间：每条放回池里的连接有自己的 keepalive 任务，闲置
  `EngineTuning::keepalive`（4 分钟）后取出来发一次 NOOP，成功（或只是被拒绝）就放回去，重新计时；
  连接已断开就关掉，下次请求直接新建连接，不必先在死连接上碰一次。间隔取 4 分钟，是因为以前按
  5 分钟丢弃闲置连接时，这么久的安静连接一直是好的；服务器自己踢掉闲置连接要久得多（Gmail 约 30
  分钟）。池里按"上次被请求用完的时刻"排，取的时候拿最近用过的那条（多半还开着要的文件夹），keepalive
  放回时保留这个时刻，不会把它排到前面。连接在等的时候被取走了，就 abort 它的任务，放回来时再起一个；任务按自己的 task id 认出
  它看管的那条连接。任务只弱引用引擎，引擎关闭后自行结束；账户删除时一并 abort。连接数不变：每账户仍最多 2 条。保活不设闲置
  上限：程序还在运行（托盘里也算），就要随时能打开邮件。
- 电脑睡眠前建立的连接不再用（`on_session` 按 `Engine::since_woke` 换掉，见
  [后台运行 / 系统睡眠与唤醒](background.md#系统睡眠与唤醒)）；keepalive 遇到这样的连接直接关掉，不发 NOOP。
  定时器按不计睡眠的时钟算，看不出睡过。
- 复用的连接已断开时（`Error::is_disconnected`，服务器或 NAT 关掉了空闲连接），换一条**新建**的
  连接重试一次（池里其余的多半也已断开）；新连接上的失败直接返回，被拒绝的命令不会再发一遍。
- 按需下载（`body::on_server`）在连接已经用 EXAMINE 打开着同一个文件夹、且 UIDVALIDITY 与本地存的
  一致时，不再 EXAMINE（`Session::examined`）：Gmail 上几乎所有邮件都在 All Mail，每封省一个往返
  （约 0.5 s）。依据是 RFC 3501 §2.3.1.1：一个文件夹打开期间 UIDVALIDITY 不会变，服务器重建了选中的
  文件夹就断开连接（Dovecot 回 BYE），复用的连接因此换成新连接、重新 EXAMINE 并报 `OutOfSync`。
  用 SELECT 打开的不算：读取只在 EXAMINE 打开的文件夹上做，即使 `BODY.PEEK` 本身也不会改动什么。
  同步、草稿上传等仍然每次 EXAMINE：它们要的是最新的 EXISTS、UIDNEXT 和 HIGHESTMODSEQ。
- 删除账户时关闭连接池（`Pool::close`）：在池子的锁内清空、并记下"已关闭"，之后放回的连接一律
  丢掉。放回也在这把锁内检查，所以不会有连接在账户删除后留在池里。

## 认证

`catmail_proto::Credentials { username, password }` 是协议层看到的全部，IMAP 和 SMTP 共用。只支持
密码登录，不做 OAuth2（原因见 README 的"范围"）。IMAP 有 `AUTH=PLAIN` 就用 `AUTHENTICATE PLAIN`，
否则 `LOGIN`（`LOGINDISABLED` 时报错）；SMTP 优先 `AUTH PLAIN`，其次 `AUTH LOGIN`。

密码一路是 `secrecy::SecretString`（core 重新导出）：从 CLI / GUI 读入、`MailClient::add_account`、
`SecretStore`、`Credentials` 到 SASL 响应和 SMTP 的 AUTH 行（`Zeroizing`），`Debug` 输出不含它，释放时
清零。写给 socket 的副本发出后也清零；GUI 输入框里的文本不归我们管，这只是尽力而为：目标是不让密码散落在
日志和随手的格式化里，而不是对抗能读进程内存的人。

同步和发信在 `catmail-core/src/connection.rs` 里从 keyring 读出账户的密码，组成 `Credentials`。

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
