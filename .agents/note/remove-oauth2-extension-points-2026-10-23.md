# 移除为 OAuth2 预留的认证扩展点

- 日期：2026-10-23
- 类型：重构

## 背景

原计划在 M6 支持 OAuth2（Gmail / Microsoft 365），代码里为此预留了扩展点：只有一个变体的
`AuthKind` 和 `accounts.auth_kind` 列、按 `AuthKind` 分派的 `catmail-core/src/auth.rs`、
`#[non_exhaustive]` 的 `Credentials` 枚举，以及文档里"加 OAuth2 只需……"的说明。

现在决定不做 OAuth2：公开发布的 OAuth2 客户端要通过服务商的审核与认证（Gmail 读写邮件属于受限
范围），而 Gmail 等服务商提供应用专用密码，用 IMAP / SMTP 密码登录即可。只接受 OAuth2 的服务
（Microsoft 365、Outlook.com）不支持。

## 决策

- 扩展点整体删掉，不留"以后可能用得上"的抽象：只有一种登录方式，分派层只会让读代码的人以为还有别的。
- `Credentials` 从枚举改成结构体 `{ username, password }`，IMAP / SMTP 的 `login` 不再 match。
  `Connection::authenticate` 仍接受机制参数：它是通用的 IMAP 命令封装，不是为 OAuth2 留的。
- 项目还没发布，直接改 `0001_init.sql` 删掉 `auth_kind` 列。
- CLI `--json` 的账户输出去掉 `auth` 字段（恒为 `"password"`，没有信息量）。
- M6 只剩 Windows / macOS 打包。

## 方案

- `catmail-types`：删除 `AuthKind`，`Account` / `NewAccount` 去掉 `auth` 字段。
- `catmail-store`：删除 `auth_kind` 列及读写；测试里的建表 SQL 同步修改。
- `catmail-proto`：`Credentials` 改为结构体，`sasl_plain` 改成方法；IMAP / SMTP 登录逻辑不变。
- `catmail-core`：删除 `auth.rs`，从 keyring 读密码直接写进 `connection.rs` 的 `account_credentials`
  （不再为此复制整个 `Account`）；`check_login` 直接构造 `Credentials`。
- CLI / GUI / 测试：去掉 `auth: AuthKind::Password`；`catmail_proto::imap` 不再重新导出 `Credentials`
  （IMAP 与 SMTP 共用，只从 crate 根导出）。
- 文档：README 的范围与规划表、`architecture.md`、`protocol.md`、`gui.md`、`sending.md`、`storage.md`
  里与 OAuth2 有关的表述。Gmail 自动存已发送导致 APPEND 重复的已知问题，用应用专用密码登录
  的账户现在就会遇到，不再以 OAuth2 为前提。
- 行为不变，现有的登录测试（假服务器与 Dovecot）覆盖；`cargo fmt`、`clippy -D warnings`、全部测试通过。
