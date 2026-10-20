# 修复 QQ 邮箱每月 1–9 号的邮件同步不到

- 日期：2026-10-20
- 类型：修复

## 背景

QQ 邮箱账户的收件箱里，服务器提供 4 封邮件（`EXISTS 4`），本地只有 10-12 那一封。trace 日志显示
QQ 把 INTERNALDATE 发成 `"2-Oct-2026 08:13:27 +0800"`，但 RFC 3501 的 `date-day-fixed` 要求
`" 2"` 或 `"02"`。imap-codec 因此拒绝整条 FETCH 响应，同步不带 BODYSTRUCTURE 重试时也一样失败，
于是每月 1–9 号的邮件在所有文件夹里都缺失，那些文件夹的游标也从来没写过。

## 决策

- imap-codec 没有对应的 quirk 开关；fork 或 patch 它不值得，所以在连接层把字节交给 imap-next 之前
  修补。
- 只改响应行，literal 原样通过：literal 是邮件内容，长度也必须和声明的一致。判断 literal 用
  imap-codec fragmentizer 的同一条规则，两边才不会错位。
- 考虑过第二次重试时不要 INTERNALDATE：一个月里差不多三分之一的邮件每次都要多拉两遍，而且会丢掉
  INTERNALDATE，所以没有采用。

## 方案

- 新增 `crates/catmail-proto/src/imap/quirks.rs`：`Quirks` 按行缓存，行完整后补上日期前的空格，再按
  行尾的 `{n}` 透传 literal。`Connection::read_untimed` 读到的字节都先经过它。
- 评审指出：修补层等换行时会无限缓存，绕过 imap-next 对单个响应 100 MiB 的上限。现在一行超过 64 KiB
  还没结束就先交出前面的部分（先修补，只留最后 15 字节，被截断的 `INTERNALDATE "2-` 不超过这么长），
  测试 `a_line_without_an_end_is_passed_on`、`a_long_line_is_still_repaired_and_its_literal_found`。
- 再次评审指出：literal 长度可以带任意多个前导零（`{000…5}`，imap-codec 接受），只留 15 字节会丢掉 `{`，
  literal 的内容就被当成响应行修补、长度被破坏。改为逐字节跟踪 literal 声明（`Announcing`，照搬
  fragmentizer 的状态机），不再从行尾解析；测试 `an_announcement_longer_than_what_a_long_line_keeps`
  （分段时丢掉状态就失败）。
- 单元测试覆盖大小写、合法日期不变、literal 里的同样文字不被改、按任意方式切分读取；
  `catmail-proto/tests/imap_session.rs` 新增假服务器测试 `reads_internaldate_with_a_one_digit_day`
  （没有这次修复时失败）。
- 拿用户数据库的副本对真实的 QQ 邮箱同步：INBOX 4 封全部入库，所有文件夹都写上了游标，不再有
  "skipping malformed server response"。
- `docs/architecture/protocol.md` 连接层一节记录了这个修补层。
