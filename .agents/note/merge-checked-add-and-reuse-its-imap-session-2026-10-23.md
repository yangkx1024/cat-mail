# 合并添加账户 / 身份的检查入口，复用检查时的 IMAP 连接

- 日期：2026-10-23
- 类型：重构

## 背景

#91：`MailClient` 添加账户和身份各有三个公开入口（`check_*` 只检查、`add_*` 只存、`add_*_checked`
先检查再存），前端只用后两种，`check_*` 只剩测试在用；checked 路径上输入校验和密码检查各跑两遍，
文档要讲三种。

#98：checked 添加时登录 IMAP 检查密码、读出 APPENDLIMIT 后就 LOGOUT，紧接着启动的后台同步任务从
没有连接开始，再做一遍 TCP + TLS + LOGIN；用户添加账户后等收件箱，要多等一次握手（Gmail 常要一秒左右）。

## 决策

- 每种一个入口：`add_account(new, password, Verify)`、`add_identity(account, new, password, Verify)`，
  `Verify::Login` 先登录、失败什么都不存，`Verify::Skip` 不连服务器。公开的 `check_*` 和 `*_checked`
  都去掉，测试改用 `Verify::Login`。`Verify` 放在 core（`client.rs`），它是引擎行为的参数，不是领域数据。
- 检查登录好的 IMAP 连接在账户插入（id 确定）之后用 `return_session` 放进连接池，watch 任务启动时先
  `take_session`：不给 `watch::spawn` / `run` 加参数，也不在里面分有没有后台同步的情况；没有后台同步时
  它留给下一个按需操作（`with_session`）。连接是刚登录、什么都没选中的，与 `connection::connect` 新建的
  一样（APPENDLIMIT 已随插入写进去）。考虑过直接把连接传给 watch 任务，两个入口多一个只有一处用的参数，放弃。
- 检查失败（SMTP 拒绝）或插入失败（地址已存在）时先 LOGOUT 再返回错误，连接不留，也不丢下一条
  不打招呼就断的连接。
- 只复用 IMAP。身份只连 SMTP，发信每次新建 SMTP 连接，没有可交的地方。
- GUI 添加后照旧调 `sync_account`（它总是用自己的连接，见 protocol.md 连接池），没有改：后台任务现在
  不用握手就开始全量同步（INBOX 在前），`sync_account` 跳过它已同步过的文件夹，收件箱不再等新登录。
  去掉 GUI 这次多余的同步会丢掉"同步中"指示和失败通知，不在这次范围内，记在 #101。
- CLI 的 `--no-verify` 提示只在登录检查失败时加（`Login::added`），`Verify::Skip` 的错误照原样报。
- CLI 的 `account add` 没有后台同步，放进池的连接随进程退出断开，不再 LOGOUT；池里的连接本来就这样。

## 方案

- `catmail-core`：`client.rs` 新增 `Verify`；`add_account` / `add_identity` 在 `rt.run` 里先按 `Verify`
  检查，再 `blocking` 插入，提交后发事件；`check_account` / `check_identity` 成为自由函数，前者返回大小上限和 IMAP 连接；
  `watch::run` 第一条连接先从池里拿。`lib.rs` 导出 `Verify`。
- CLI（`cmd/account.rs` 的 `Login::verify` / `added`、`cmd/identity.rs`）、GUI 两个表单、各处测试跟着改签名。
- 文档：`cli.md`、`gui.md`、`sending.md`、`sync.md`。
- 测试：`sync.rs` 的 `an_account_is_added_only_once_logged_in`（错密码、空密码、SMTP 拒绝、地址已存在
  都不存，后两种 IMAP 最后是 LOGOUT）、`an_account_added_once_logged_in_keeps_that_connection`（无后台同步：
  下一次按需同步用检查留下的连接）；`watch.rs` 的
  `an_account_added_once_logged_in_is_watched_on_that_connection`（后台任务到 IDLE 时假服务器只接受过一条
  连接）；`identities.rs` 的 `identities_are_added_only_once_logged_in`。不把连接放进池时两个连接数测试都失败。
