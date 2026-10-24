# 添加账户后的同步等后台首次完整同步，不另开连接

- 日期：2026-10-24
- 类型：修复

## 背景

issue #101：开着后台同步时，GUI 添加账户后还自己调 `sync_account`。后台任务已经拿着检查密码那条连接
开始第一次完整同步，`sync_account` 却总是新建连接：多一次 TCP + TLS + LOGIN（添加时一共登录 2 次），
多一次 LIST 和每个文件夹的检查。GUI 这次同步不能直接删：它驱动侧栏的"Syncing…"和文件夹失败的通知。

## 决策

- 新增 `MailClient::initial_sync`：后台任务第一次完整同步进行中时等它做完，直接返回它的 `SyncReport`；
  否则或它失败了就是 `sync_account`。GUI 添加账户后改调它（`sync_accounts(.., initial: true, ..)`），
  侧栏的指示和失败通知照旧。
- 最初让 `sync_account` 本身去等，Codex review 指出三个问题，于是拆成单独的方法：
  - 用户要求的刷新会拿到调用之前就开始的那次的报告，那次已做完的文件夹其间的新邮件要等下一次完整同步；
    首次同步本身也可能跑好几分钟，只等第一次挡不住。
  - 后台同步用 `Uploads::Settled`，刚编辑的草稿不上传；用户要求的同步本应全部上传。
  - 等到的成功又经 `Engine::report` 记一遍，会清掉其间更新的错误；清掉"登录被拒"还提前唤醒后台同步。
    `initial_sync` 等到的结果不再记。
- 放弃 issue 里的"引擎发布后台同步状态的事件、GUI 据此显示"：GUI 要自己记哪些账户在等首次同步、在状态
  变化里找"做完"那一刻再发通知，和 `Added` 的先后有竞态，改动也大得多。
- 放弃"`sync_account` 用池里的连接"：开着后台同步时那条连接已被后台任务拿走，没用。
- 放弃"后台任务接管所有用户要求的同步"（唤醒它、等下一次完整同步）：更通用，但用户的同步用自己的连接
  是有意的，免得被后台连接的退避或无声断开的 IDLE 卡住。
- 后台首次同步失败时不转交错误（`Error` 不能 clone），等着的调用改用自己的连接再同步一次。
- 登记在 spawn 任务之前做（`watch::task` 里 `Engine::start_first_sync`），`add_account` 返回后马上调用的
  一定找得到。报告在 `sync::run` 一返回时交出（`Engine::first_sync_done`），不等后面的 INBOX 步骤；
  第一步结束时 `FirstSync` 的 `Drop` 撤下登记，任务以任何方式结束都不会让调用一直等。

## 方案

- `engine.rs`：`AccountState.first_sync`（`watch::Sender<Option<SyncReport>>`）、`FirstSync`、
  `start_first_sync`、`first_sync_done`、`first_sync`。
- `watch.rs`：`task` 先登记再返回任务；`work` 完整同步后交出报告，`run` 在第一步之后丢掉 `FirstSync`。
  `client.rs` 启动时也走 `watch::task`。
- `sync.rs` / `client.rs`：`initial_sync`。GUI：`mailbox/commands.rs`、`mailbox/mod.rs`。
- testkit：`Faults::fetch_gate`，像 `body_fetch_gate` 那样扣住头部 FETCH。
- 文档：`docs/architecture/sync.md`（后台同步的"首次完整同步"），`docs/architecture/gui.md`。
- 测试：
  - `client.rs` 单元测试：首次同步被 `fetch_gate` 扣住，等 `Engine::first_sync_waiters`（`cfg(test)`）
    看到调用已在等再放行，只有 1 条连接、1 次 LIST；不等时调用看不到登记，测试超时失败。放在单元测试里
    是为了拿到这个明确的"已在等"信号：集成测试只能隔一段固定时间放行，调度慢时会偶发失败。
  - `tests/watch.rs`：首次同步之后的 `initial_sync` 自己连接；首次同步登录被拒时等着的调用自己再登录一次
    并报 `Auth`。
