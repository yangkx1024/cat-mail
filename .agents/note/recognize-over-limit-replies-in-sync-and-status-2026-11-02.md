# 同步与账户状态认出服务器的下载限额回复

- 日期：2026-11-02
- 类型：修复

## 背景

#199：#195 的后台预取认得服务器的"超出下载限额"回复（Gmail 的 `[ALERT] Account exceeded command or
bandwidth limits`，网易的 `Fetch volume limit exceed`），同步和打开邮件不认得：

- Gmail 在登录时回超限，`imap::Error::AuthFailed` 被提升为 `Error::AuthFailed`，账户状态成了"登录被拒"，
  GUI 让人去改密码；后台同步按登录被拒等 `full_sync`，直到用户要求的同步成功。
- 网易在 FETCH 上回超限，打开邮件报"服务器拒绝"（`Refused`），看不出是限额；同步把它当成单个文件夹的拒绝，
  接着逐个文件夹去碰。
- 其他时候后台同步按普通失败从 30 秒起退避，每次重试都可能算进服务器的计数。

## 决策

- **新 `ErrorKind::OverLimit` 和 core 的 `Error::OverLimit(String)`**（带服务器原话）。`From<imap::Error>`
  最先认它，不论来自登录、BYE 还是命令的 NO：只加 kind 不够，`AuthFailed` 的文字是"检查用户名和密码"。proto
  加 `imap::Error::over_limit()` 取出原话，`says_over_limit` 改回私有；proto 的 `kind()` 也返回 `OverLimit`，
  保持一致。
- `is_transient()` 为真（什么都不改、稍后会好；`ops flush` 因此退出码 3），`is_refusal()` 为假：同一连接上下一条
  命令不会更好，同步就此停下，不再逐个文件夹去碰。proto 的 `imap::Error::is_fatal()` 也算它：操作回放、草稿上传
  直接看 proto 错误，否则网易拒绝 STORE / MOVE 时会把操作记成"服务器拒绝"（永久失败，撤销用户的改动）。
  `imap()` 不返回它，所以连接池不会因为超限的 BYE 换新连接再登录一次；core 的 `Error::breaks_connection()`
  （超限或 `is_fatal`）让连接池、预取都不留碰上超限的连接（keepalive 只会添数）。
- **后台同步**：新增 `SyncSchedule::over_limit`（默认 1 小时）。考虑过照预取停 24 小时，但 Gmail 通常一小时
  就解除，同步停一天代价太大；每小时一次登录对限额可以忽略。本地改动不叫醒它（`Engine::ignores_wakes`），
  否则用户在锁定期间每次标记已读都是一次登录；用户要求的同步（`syncing` 置位）照样去试，用户在等；睡眠唤醒
  也去试：tokio 定时器没算睡着的时间，限额多半已过。成功（任何同步或回放）清除状态时像登录被拒一样立即唤醒
  后台同步（`record` 返回的 `rejected` 改为 `waited`）。"登录被拒或超限"合成 `ServerProblem::waited_out()`，
  `record` 和预取（`Engine::waits_out`）共用；`ignores_wakes` 另写，因为用户要求的同步只对超限放行。`Failures` 里超限也算连上了服务器，不走醒来后的快速重试。
- **账户状态**：`ServerProblem::over_limit()`；GUI 侧栏显示 "… is over the server's download limit for now"，
  CLI JSON 的 kind 是 `over_limit`，`catmail watch` 打印错误原文（`Error::OverLimit` 的文字说明会自行解除）。
- **预取**：同步报告超限时不开始（`may_start`），等同步再次成功（账户消息）就继续，不再另停 24 小时；
  预取自己撞上超限时仍按原设计停 24 小时、不改账户状态。
- 没做：SMTP 的发送限额（Gmail 的每日发信上限）不在本 issue 范围内。

## 方案

- `catmail-types`：`ErrorKind::OverLimit`、`ServerProblem::over_limit` / `waited_out`。
- `catmail-proto/src/imap/error.rs`：`over_limit()`、`kind()`、`is_fatal()`。
- `catmail-core`：`error.rs`（新变体、提升、`is_transient`、`breaks_connection`）；`engine.rs`（`waits_out`、
  `ignores_wakes`、`sync_problem`、`reached`、`record`）；`draft_sync.rs`（超限的 APPEND 拒绝不算上传失败）；`watch.rs`（`over_limit` 间隔，`unless_rejected` 泛化为 `unless`）；
  `pool.rs`（不留超限的连接）；`prefetch.rs`（`may_start`）。
- 前端：`catmail-gui` 侧栏、`catmail-cli` JSON。
- `catmail-testkit`：新故障 `login_refusal`（所有登录回 `NO <text>`）。
- 文档：`architecture.md` 错误一节，`sync.md`、`background.md`、`bodies.md`、`gui.md`、`cli.md`。
- 测试：core 单元测试（三种来源都提升为 `OverLimit`、transient、非 refusal）；`sync.rs` 登录超限不算登录被拒；
  `bodies.rs` 打开邮件碰上网易的超限报出能看懂的错误、连接不留；`watch.rs` 超限后本地改动不触发登录、用户要求
  的同步试一次、解除后继续，以及过了 `over_limit` 再试；`prefetch.rs` 同步报告超限时预取不开始、同步成功后继续；
  `ops.rs` 改动被以超限文字拒绝时留在队列里、解除后照做。两家的原话放在 `tests/common` 的常量里。
  对 `over_limit` 间隔、`ignores_wakes`、连接池丢弃、`may_start`、`is_fatal` 做了变异测试，对应测试都会失败。
- **codex review 修复**（2 条，均先写出不修就失败的测试）：
  - IDLE 的错误是 proto 原始错误，在提升为 `Error::OverLimit` 之前就被单独处理：以超限拒绝的 IDLE 落进"服务器
    拒绝 IDLE"分支，从此改为轮询（限额解除后也不恢复）；以超限结束的 BYE 当成断线立即重连、多一次登录。现在
    这两种先认出超限，记进账户状态（`Activity::Sync`）、等 `over_limit`。testkit 新故障 `idle_refusal`、
    `bye_on_idle`；测试 `an_idle_ended_over_the_limit_waits_and_idles_again_once_lifted`。
  - 别的同步发现超限时后台同步正在 IDLE，本地改动的唤醒由 IDLE 里的 `stopped` 直接收下，不看
    `ignores_wakes`，多做一步。现在 IDLE 中也按它过滤；`ignores_wakes` 加参数 `logged_in`：IDLE 中登录被拒不挡
    唤醒（连接早已登录，原有行为不变）。测试 `a_change_while_idling_waits_once_another_sync_found_the_account_over_its_limit`。
- **/simplify 整理**：超限并入 proto 的 `is_fatal`（原来连接池、预取各有一处特判，回放还漏了）；去掉 core 的
  `is_over_limit`、`Engine::over_limit`；测试等待函数泛化为 `wait_for_problem`。没采纳：把 testkit 的
  `reject_login: bool` 与 `login_refusal` 合成一个（要改现有调用点，收益小）；去掉 proto `kind()` 里的超限分支
  （core 用不到，但直接用 proto 的人需要它保持一致）。
