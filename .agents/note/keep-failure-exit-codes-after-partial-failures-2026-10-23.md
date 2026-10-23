# 部分失败后保留 CLI 的失败退出码

- 日期：2026-10-23
- 类型：修复

## 背景

PR #75 合并后 codex review 指出两处退出码 3（"稍后重跑即可"）用错了，核实后还发现一个相关的 core bug：

- SMTP 拒绝登录或不支持 STARTTLS 时，`send::pass` 把排队的邮件推迟回 `queued` 并返回成功，CLI 只看条目
  状态，`send` / `outbox flush` 以 3 退出，并提示重跑，但密码不改重跑没用。复现：假 SMTP `reject_login`。
- 回放先拒绝了一个改动（已提交并在本地撤销），后面的文件夹断线时，`ops::replay` 丢掉已累计的计数直接
  返回错误，CLI 只看到暂时性错误，以 3 退出，而被拒绝的改动重跑也不会回来。同一文件夹内前一批被
  拒绝、后一批断线时也一样（`replay_folder` 的计数只在它成功时才交出）。草稿上传同理。
- 更糟的是复用的连接中途断开时，`on_session` 在新连接上整个重来，第一次尝试的拒绝在第二次已不是 pending，
  报告里 `failed == 0`，命令以 0 退出。

## 决策

- `ReportedError` 加 `transient`（`Error::is_transient()` 的结果）：单看 `kind` 分不出 SMTP 421 和 554。
  CLI 发信后读 `account_status().send`，问题不是暂时的就把仍排队的邮件算成 1，并换成"账户能用后
  `outbox flush`"的提示。考虑过让 core 的 flush 在这种情况下返回错误，但那会让 GUI 后台发信丢掉推迟
  条目的结果，没采用。
- `replay_pending` 改为连不上 / 断线时也返回 `Ok`，原因放进新的 `ReplayReport.error`，计数是之前做了的
  （参照 `FolderSyncReport.error`）；只有账户被删除或读本地数据失败才返回 `Err`。这样拒绝不会因为后面
  断线而丢失，CLI 也不必去比对前后的队列。
- 计数跨尝试累加：每次尝试从 `Arc<Mutex<_>>` 里取出同一个 `Replayed`，做完放回（不跨 `.await` 持锁；
  闭包借用局部变量会让 future 不是 `Send`，见 `with_session`）。草稿按 ID 记（`Uploaded { saved, refused }`）：被拒绝的草稿每次回放都会重试，第一次
  被拒绝、重来时存上的不算被拒绝，单纯相加会重复计数。

## 方案

- types：`ReportedError.transient`、`ReplayReport.error`。
- core：`draft_sync::upload`、`ops::replay` / `replay_queued` / `replay_folder`（`Settle` 持有文件夹
  计数的引用）改为向调用方给的累加器里加（失败前做的也在）；`replay_account` 跨尝试合并并填 `error`；
  同步和后台任务的调用随之调整，行为不变。
- CLI：`ops` 的回放按 `report.error.transient` 决定 3 / 1，`--json` 的回放结果加 `error`；`outbox` 的
  `blocked()` 读账户的发信问题；`save_to_server` 照旧警告。
- testkit：`refuse_next_changes`（拒绝接下来的 n 条改动命令），与 `drop_before_change` 一起造出"先拒绝、
  后断线"。
- 测试：`ops::refusals_before_a_lost_connection_are_reported`（跨文件夹和同一文件夹）、
  `drafts::refused_uploads_before_a_lost_connection_are_reported`（都覆盖重来成功和不再重来两种），
  CLI 的 `ops::a_refusal_before_a_lost_connection_is_a_failure`（在 main 上得到 3）和
  `outbox::exit_code_says_how_a_send_ended` 补 SMTP 拒绝登录为 1。
- 文档：`architecture.md`（`ReportedError`）、`op-queue.md`（回放的报告）、`cli.md`（退出码、JSON）。
