# 登录后发 IMAP ID，网易邮箱才肯打开邮箱

- 日期：2026-11-02
- 类型：修复

## 背景

#197。163 账户登录成功，但所有文件夹拉不到邮件。日志里每个文件夹的 EXAMINE 都被拒：
`EXAMINE failed: EXAMINE Unsafe Login. Please contact kefu@188.com for help`。网易（Coremail）要求客户端
先发 `ID`（RFC 2971）报上名字，才允许 SELECT / EXAMINE；catmail 从来不发。用真实的 163 账户复现过；
`imap.163.com` 登录前的 CAPABILITY 里就有 `ID`，也正常回答 `ID`。

## 决策

- 放在 proto 的 `Session::login` 里（认证之后，PREAUTH 也发），而不是 core 的 `connection::login`：
  每条连接（同步、连接池、后台同步、预取、测试里直接用 `Session` 的）都经过它，不用调用方记着。
- 只在服务器宣称 `ID` 时发；只报 `name` 和 `version`（`CARGO_PKG_VERSION`，工作区统一版本），不带操作
  系统等信息。
- 服务器的回答无关紧要：NO / BAD 只记 debug 日志，用 `execute_lossy` 读，解析不了的 `* ID` 也不算失败；
  连接断开、超时、BYE 照常报错。
- 不只对网易发：Gmail、Dovecot 等也宣称 `ID`，多一个往返，但连接池复用连接，163 登录本身要 2–4 秒，
  这点代价可以忽略；按主机名特判更脆弱（网易有多个域名，也有企业邮箱）。

## 方案

- `catmail-proto/src/imap/session.rs`：`login` 拆出 `authenticate`，之后调 `identify`。
- `catmail-testkit/src/imap.rs`：假服务器默认宣称并回答 `ID`（像真实服务器）；新增 `Faults::id`、
  `id_refusal`、`require_id`（没发 `ID` 的连接 EXAMINE / SELECT 回网易那句 `NO … Unsafe Login`）。
- 测试：`imap_session.rs` 的 `introduces_itself_to_servers_that_ask`（含 PREAUTH、没有 `ID` 能力时不发）、
  `a_refused_or_unreadable_id_answer_is_ignored`；`catmail-core/tests/sync.rs` 的
  `syncs_servers_that_want_the_client_to_introduce_itself`（同步和回放都通，每条连接都发了一次 `ID`）。
  去掉修复后前两个测试失败。
- 文档：`docs/architecture/protocol.md` 的"认证"一节。
