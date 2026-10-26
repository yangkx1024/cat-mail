# 按身份记下服务器是否自动存已发送

- 日期：2026-10-26
- 类型：功能

## 背景

Issue #135：Gmail 会自动把经它的 SMTP 发出的邮件存进已发送，#128 用启发式跳过我们的 APPEND（账户模式是
Gmail、同一个登录、同一个域）。缺口：第一次同步之前模式还是 standard，发出的会多存一份；域名判断可能误判
（`smtp-relay.gmail.com`、别的也自动存的服务商）；用户没法纠正。

## 决策

- 身份上加 `saves_sent`（`identities.saves_sent`，可空）：`Some` 是用户说的，之后不再覆盖；`None` 时由
  `Identity::server_saves_sent(&Account)` 现猜。issue 原计划存一个布尔默认值（添加身份时、第一次认出 Gmail
  时各写一次）并另记"用户改过"：那样要在两处写默认值、多一个标记，而存下的值总等于现猜的结果，还消不掉
  "第一次同步之前"的缺口（那时也还不知道是 Gmail），保存时照样要看会话。就改成可空、现猜。
- 猜测看服务器是不是 Gmail（`accounts.gmail_server`，`X-GM-EXT-1`），不再看账户模式："所有邮件"被隐藏
  的 Gmail 账户也自动存已发送。
- 账户还没同步过（记下的 `gmail_server` 为 false）时，`attempt_save` 在拿到 IMAP 会话后看它有没有
  `X-GM-EXT-1`，按 Gmail 再猜一次。不在这里写 `gmail_server`：模式还是 standard，写了会让账户暂时显示成
  "所有邮件被隐藏"。已知是 Gmail 且会自动存的，照旧不开会话。
- CLI 用 `identity saves-sent <id> yes|no|auto`，`auto` 放回去重新猜；GUI 只有一个开关，拨动即存下用户的
  选择，不提供"放回自动"：少一个控件，CLI 有它。
- #128 里"用测试 Gmail 账户确认经 SMTP 发出的会自动进已发送"仍没验证（没有测试账户），猜错时用户可以改。

## 方案

- types：`Identity::saves_sent`、`Identity::server_saves_sent`（原 `send.rs` 的 `saves_sent_mail`，改看
  `gmail_server`；`server_saves_sent_if` 由调用方给出服务器是不是 Gmail），单测猜测与用户设置优先。
- store：`0001_init.sql` 加 `identities.saves_sent`；`Identities::set_saves_sent`，单测。
- core：`MailClient::set_identity_saves_sent`（与 `set_default_identity` 共用 `update_identity`，发 `IdentitiesChanged`）；`send::attempt_save` 读身份的
  设置，未同步时看会话的 `X-GM-EXT-1`。
- CLI：`identity saves-sent`；`identity list` 多一列 SAVES SENT（猜的标 `(auto)`）；JSON 的
  `server_saves_sent`、`server_saves_sent_auto`。
- GUI：Settings › Accounts 每个身份下一个"Server saves to Sent"开关。
- 文档：`sending.md`"保存到 Sent"、`labels.md`"草稿与已发送"、`cli.md`、`gui.md`。
- 测试：core `tests/gmail.rs` 未同步的 Gmail 账户不多存一份、用户说不存之后存一份且同步不覆盖；
  `tests/send.rs` 用户说服务器会存时不存、放回 `None` 后照常存；CLI `saves-sent` 设置与放回；GUI 开关。
