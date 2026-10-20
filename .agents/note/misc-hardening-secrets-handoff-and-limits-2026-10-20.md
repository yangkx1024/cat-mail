# 杂项加固：密码类型、事件脱敏、交接页清理、响应总量与主题导入上限

- 日期：2026-10-20
- 类型：修复

## 背景

Issue #25 汇总的六项信息级加固，单独都不值一个 PR：

1. 密码一路是普通 `String`，`Credentials` 派生 `Clone`，`sasl_plain` 产出含密码的 `Vec<u8>`，都不清零。
2. GUI 登录表单的 `Values` 派生 `Debug` 且含密码，离 `tracing::debug!(?values)` 只差一行。
3. `imap/connection.rs` 把整个 imap-next 事件 `{other:?}` 写进 `Error::Protocol`；imap-types 的
   `Secret` 只在 release 下脱敏。目前 LOGIN 走不到这些分支，但改动匹配臂或换认证方式就会泄露。
4. 交给浏览器的邮件页面在 `handoff/` 里留到下次 `MailClient::open`，GUI 开几天就留几天。
5. 一条 IMAP 命令可累积的 untagged 响应条数不限（imap-next 只限单条 100 MiB），恶意服务器可撑爆内存。
6. 主题导入直接 `fs::read` 用户选的文件，没有大小上限。

## 决策

- 密码用 `secrecy::SecretString`（新依赖，0.10.3；`zeroize` 已在依赖树里），core 重新导出
  `SecretString` / `ExposeSecret`，前端不直接依赖 secrecy。`SecretStore::get/set` 也改成
  `SecretString`，keyring 返回的 `String` 立刻包起来。SASL 响应和 SMTP AUTH 行用 `Zeroizing`。
  交给 imap-next 的副本、GUI 输入框里的文本管不到，所以只是尽力而为，文档里写明。
- 第 2 项随第 1 项解决：`SecretString` 的 `Debug` 输出是脱敏的，`Values` 保留 `derive(Debug)`；
  `Credentials` 手写的 `Debug` 也因此改回 derive。
- 第 3 项只格式化事件变体名；`event_name` 穷举匹配（`Event` 不是 `non_exhaustive`），imap-next 加变体时
  编译报错。greeting 前的 debug 日志同样改掉。这条路径目前走不到，没有加回归测试。
- 第 5 项按字节而不是条数计：一条命令发出后读到的字节超过 256 MiB 即返回 `Error::Protocol`（致命），并
  置 `out_of_step`，剩下的回答不会被当成下一条命令的。上限取 256 MiB：最大的正常回答是一整封邮件，
  imap-next 本身限 100 MiB。只在 `next_event` 里检查，IDLE 期间不攒数据，不受限。
- 第 4 项：`handoff/` 的生命周期本来就归 core 管（打开时清空），所以新增 `MailClient::hand_off`，
  前端不再自己往目录里写文件。每次交接时删掉修改时间超过 10 分钟的文件，删不掉（Windows 上浏览器还开着）
  就算了。考虑过定时器清理，但交接时顺手清已经足够，不必多一个后台任务。
- 第 6 项上限 4 MiB，用 `take` 读，超过给出用户可见的错误。`drop_trailing_commas` 的 O(n·k) 有了上限
  就无所谓，没改。

## 方案

- `crates/catmail-proto/src/credentials.rs`、`imap/session.rs`、`imap/connection.rs`、`smtp/session.rs`：
  `SecretString` / `Zeroizing`；`event_name`；`MAX_ANSWER` 与 `received` 计数。
- `crates/catmail-core/src/{secrets,auth,client,lib}.rs`：`SecretStore`、`check_login`、`add_account` 改签名；
  `MailClient::hand_off`（tempfile 从 dev 依赖改为正式依赖）。
- `crates/catmail-cli/src/main.rs`：`read_password` 返回 `SecretString`，原地截掉换行。
- `crates/catmail-gui/src/mailbox/add_account.rs`、`reader/mod.rs`、`theme/appearance.rs`。
- 文档：`docs/architecture.md`、`docs/architecture/protocol.md`、`docs/architecture/gui.md`。
- 测试：`an_answer_over_the_limit_is_given_up` / `the_limit_is_per_command`（duplex 上的脚本化服务器，
  把上限调小）、`debug_output_leaves_out_the_password`、
  `handed_off_files_are_removed_once_old`、`oversized_files_are_not_imported`。
