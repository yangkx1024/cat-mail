# 引擎提供"现在同步"，GUI 不再自己逐个账户同步

- 日期：2026-10-29
- 类型：重构

## 背景

#152：GUI 里"同步所有账户"有两份（`Mailbox::sync_accounts`、托盘在窗口关着时的 `Resident::sync`），
都另起 `sync_account`，与引擎本来就在跑的后台同步并行；添加账户后还有 `initial_sync` 去等后台同步的
首次完整同步。以后给"同步所有账户"加东西，每份都得改。

## 决策

- 引擎提供 `MailClient::sync_now()`，由每账户的后台任务来做，不另开连接。要求的时刻记成一个截止点
  （与醒来时刻同一个 `Cutoff`）：开始于它之前的完整同步不再算数，所以任务的下一步就是完整同步，
  正在进行的那次（用户不在等的，如定时的或 CLI 的）做完后会再来一遍。不改醒来时刻，连接照常用（issue 的要求）；代价是 IDLE 的连接若已
  无声断开，要等 DONE 超时或 keepalive。考虑过像 `resume` 那样直接丢掉 IDLE 连接，没有采用。
- 登录被拒绝的账户不理会，与 `wake` / `resume` 相同；正在同步的账户也不再要求，进行中或待做的那次就是
  答复。这条规则只在引擎（`Engine::begin_syncing`），GUI 不再自己判断，托盘不论窗口开没开都直接调
  `sync_now`。
- "正在同步"由引擎给出：`AccountStatus::syncing`。只在用户可能在等时置位：要求之后，和任务的首次
  完整同步（启动、添加账户）；每 15 分钟的完整同步不算，免得按钮定期转圈、多发事件。开始于要求之后的
  那一步结束时撤下，与这一步的结果一起只发一次 `AccountStatusChanged`（`Activity::SyncBegun(began)`）。
  `sync_account` 也按它开始的时刻撤下。任何一次同步发现登录被拒也撤下，不论何时开始：之后后台同步等
  `full_sync`，要求等不到答复，否则"Syncing…"会盖住"无法登录"十五分钟（review 发现）。首次同步与要求走同一个 `begin_syncing`（只是不唤醒任务）。
- 首次同步并进来：GUI 添加账户后不再同步，`initial_sync` 及其等待机制（`FirstSync`、`first_sync`）
  删掉。
- `Mailbox` 的失败通知去掉，只剩侧栏的账户状态；文件夹级的失败不再弹通知（后台同步本来也只记日志）。
- 没有后台同步时 `sync_now` 返回 `Error::NoBackgroundSync`。起初它在调用里逐个 `sync_account`，但只有
  关掉后台同步的 GUI 测试走那条分支：一个方法两套行为，测试也测不到真实路径；按 Sync 的 GUI 测试改用
  开着后台同步的客户端。CLI 的 `catmail sync` 不变。

## 方案

- `catmail-core`：`engine.rs`（`Woke` 改名 `Cutoff`；每账户 `asked`；`begin_syncing`、`ask_to_sync`；
  `synced` / `set_synced` 经 `full_sync_counts` 也看 `asked`；`report` 处理 `SyncBegun`），`watch.rs`
  （每步记开始时刻，按 `SyncBegun` 报告），`sync.rs`（`sync_account` 按 `SyncBegun` 报告，删 `initial_sync`），
  `client.rs`（`sync_now`），`error.rs`（`NoBackgroundSync`）。`catmail-types`：`AccountStatus::syncing`。
- GUI：`Mailbox::sync`、`Resident::sync` 都只调 `sync_now`，`syncing()` 由各账户状态得出；
  `AddAccountEvent::Added` 不再带账户。CLI `watch` 的状态行分出 syncing / synced。
- 文档：`sync.md`（现在同步）、`gui.md`、`background.md`（托盘）。
- 测试：`tests/watch.rs` 新增首次同步显示为 syncing、现在同步在后台连接上同步全部文件夹（同步中再要求
  不多做）、开始于要求之前的完整同步再做一遍（这两个去掉 `asked` 时失败）、登录被拒时不理会、要求之前
  开始的登录被拒后不再显示 syncing（修复前失败）；testkit 的
  `ServerState::hold_fetches`、`tests/common` 的 `status_changes` 收拢了重复的测试代码；`tests/sync.rs`
  没有后台同步时 `sync_now` 失败、不置位；GUI 托盘同步在窗口开着 / 关着时的两个测试和
  `signs_in_from_the_window` 用带后台同步的客户端（`TestMail::start_syncing_in_background`、
  `without_account_syncing_in_background`）。
