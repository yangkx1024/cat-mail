# 连上服务器时就记下是不是 Gmail

- 日期：2026-10-26
- 类型：重构

## 背景

Issue #142：`accounts.gmail_server` 只在同步对账时写入，第一次同步之前的 false 既是"不是 Gmail"也是
"还不知道"。#141 的 `Identity::server_saves_sent` 按它猜，于是第一次同步之前 GUI 的"Server saves to Sent"
开关、`catmail identity list` 的 SAVES SENT 对 Gmail 身份显示"否"；`send::attempt_save` 为此另看会话的
`X-GM-EXT-1`（`server_saves_sent_if`），判断分在两处。

## 决策

- 连上就记：`connection::connect` 每次登录后记下 `gmail_server`（与 APPENDLIMIT 一起，变了才写，同一个
  事务；写不进只记日志，下次连接再写；变了发 `AccountChanged`）；
  `add_account` 的登录检查在插入账户的同一个事务里记下。同步不再写它。
- 难点是 `all_mail_hidden()` = Gmail 服务器 && 普通模式：只提前写 `gmail_server`，刚添加的 Gmail 账户在
  第一次同步前就会显示"所有邮件被隐藏"。issue 给了两条路，选了第二条：模式改成可空
  （`Account::mode: Option<AccountMode>`，`accounts.mode` NULL 到第一次同步），`all_mail_hidden()` 要
  `Some(Standard)`。第一条（登录检查时也 LIST 一次定下模式）只覆盖带登录检查的添加，`connect` 记下
  `gmail_server` 时仍会碰上同样的问题，除非每次连接都多一个 LIST。"还没同步过"本来也就是模式还不知道。
- `attempt_save` 先按记下的判断，会自动存就不开会话；拿到会话之后把账户的 `gmail_server` 换成会话的（与连接
  时记下的相同），在内存里再判断一次，照顾不检查登录添加、之前没连上过的账户。两处都是同一个
  `Identity::server_saves_sent`，`server_saves_sent_if` 去掉了。考虑过在会话里重读数据库，多一次读写往返，
  结果一样。
- 事件 `IdentitiesChanged` 改名为 `AccountChanged`（"账户变了：身份，或对服务器知道的：是不是 Gmail、模式"）。
  `all_mail_hidden` 由 `gmail_server` 和模式推出，身份的猜测按 `gmail_server`：只借用 `IdentitiesChanged`
  的话，它的说明成了一串例外，两个输入由两个事件宣告。同步改了模式时除了 `FoldersChanged` 也发它。
  GUI 侧栏收到它照旧重读账户，写信窗口重读身份；`catmail watch --json` 的事件名随之变为 `account_changed`。
- 搜索 SQL 里普通账户的条件从 `a.mode = 'standard'` 改成 `a.mode IS NOT 'gmail'`，NULL 时也排除虚拟的
  `\All` / `\Flagged`（实际上同步前没有文件夹，store 的单测会直接建）。
- 项目尚未发布，`mode` 改成可空仍保持 JSON `schema_version = 1`；首次发布后才对不兼容变更递增版本，
  `cli.md` 与 `json.rs` 的注释同步明确这一约定。

## 方案

- types：`Account::mode` 可空，`all_mail_hidden` 要模式已知，`server_saves_sent_if` 去掉；单测。
- store：`0001_init.sql` 的 `mode` 可空无默认；`set_mode` 用 `IS NOT`；`messages.rs` 搜索条件。
- core：`connection::connect` 记 `gmail_server`（`remember_limit` 并进去了）；`add_account` 记登录检查看到的；
  `sync::run` 不再写；`send::attempt_save` 与 `server_saves_sent`。`AccountMode` 不再有 `Default`。
- types：`Event::IdentitiesChanged` → `Event::AccountChanged`；core 各处、CLI `JsonEvent`、GUI 监听随之改名。
- CLI：`--json` 的 `mode` 同步前为 `null`。GUI：标签菜单按 `Some(Gmail)`。
- 文档：`sending.md`"保存到 Sent"、`labels.md`、`cli.md`。
- 测试：core `tests/gmail.rs` 带登录检查添加的 Gmail 账户第一次同步前 `gmail_server`、`server_saves_sent`
  为真、`all_mail_hidden` 为假、模式为空，发信不 APPEND；不检查登录添加的在发信连上之后记下并发
  `AccountChanged`，第一次同步定下模式时也发；隐藏/恢复"所有邮件"的现有测试照常通过。store、types 单测改为模式起初为空。
