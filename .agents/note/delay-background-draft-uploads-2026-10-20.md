# 后台同步等草稿停下来再上传

- 日期：2026-10-20
- 类型：功能

## 背景

M4 的草稿同步在开启后台同步时，每次修改都唤醒后台任务，立刻上传一份新副本（APPEND 加删除旧的）。
M4 的笔记已记为已知问题：M5 的 GUI 写信时大约每秒自动保存一次，照这样每秒都会在服务器上换一份。

## 决策

- **只影响后台同步。** 后台任务只上传 `draft_upload_delay` 内没改过的草稿（默认 15 秒，
  `MailClientBuilder::draft_upload_delay` 可改，测试设得很短）。`replay_pending`、`sync_account`
  照旧立即上传全部，CLI 行为不变；发信本来就会删掉副本，也不受影响。
- **按草稿记、记在内存里。** `Engine::draft_edited` 在建立、修改、增删附件（以及回复、转发建的草稿）
  时记下时间，删除时忘掉。按账户记更简单，但一封一直在写的信会挡住同账户其他草稿的上传。不进数据库：
  `updated_at` 只精确到秒，测试和时限都不好用；重启后没有记录的草稿直接上传，这时也没人在写它。
- **仍按状态对齐**：被留下的草稿只是这一轮跳过（`draft_sync::Uploads::Settled` 过滤 `stale` 的结果），
  不入队任何东西。
- **到期自己醒来**：后台任务每步之后算出最早一份被留下的草稿还差多久，IDLE（或轮询）的时限取它与下一次
  完整同步中较早的一个；到时结束 IDLE，下一步正常回放和上传。只有草稿开始被留下时才唤醒任务
  （`draft_edited` 返回是否新留下），让它算出到期时间；接着的修改只推后时间，任务到期醒来时重新算。
  最初每次修改都唤醒，结果每次停顿打字都结束 IDLE、回放、同步 INBOX 一遍，什么也不上传。

## 方案

- `catmail-core`：`engine.rs` 每账户的 `draft_edits`、`draft_edited` / `draft_gone` / `held_drafts`、
  `DEFAULT_DRAFT_UPLOAD_DELAY`（从 lib 导出）；`client.rs` 的 `draft_upload_delay`；`draft_sync.rs` 的
  `Uploads`；`ops::replay_queued` 与 `sync::run` 带上 `Uploads`（用户要求的传 `All`，`watch.rs` 传
  `Settled`）；`watch.rs` 按到期时间结束 IDLE；`drafts.rs` 的 `edited`。
- 文档：`docs/architecture/sending.md`"同步到服务器"、`docs/architecture/sync.md`"后台同步与 IDLE"，
  README 删去已知问题、补一句说明。

验证：假服务器测试 `background_sync_waits_for_a_draft_to_settle`（1 秒延迟，期间改三次，没有 APPEND，
第一次之后的修改不再引起 IDLE，停下后只 APPEND 一次）；去掉过滤后该测试失败（APPEND 2 次），每次修改都
唤醒时也失败（多一次 IDLE）。`uploads_asked_for_dont_wait_for_a_draft_to_settle`
（延迟 1 小时，`replay_pending` 与 `sync_account` 仍立即上传）。原
`background_sync_uploads_changes_at_once` 改为零延迟并改名 `background_sync_uploads_changes`。
