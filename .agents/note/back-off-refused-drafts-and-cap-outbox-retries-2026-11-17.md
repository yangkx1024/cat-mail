# 被拒的草稿退避重传，发信和存 Sent 的重试设上限

- 日期：2026-11-17
- 类型：修复

## 背景

#323（来自 #319 的全仓库审查）：

- 草稿上传被服务器拒绝（如邮箱满）只写 `drafts.upload_error`，`STALE` 不看它，于是每个后台步骤、每次
  `sync_folder`、每次 `replay_pending` 都把整封信（含全部附件）重新构建、APPEND 一遍。没有 IDLE 时每 2 分钟一次，
  有 IDLE 时 INBOX 每次变化就一次，浪费流量，也容易撞上服务商限额。
- 草稿同步只要 `need_upload` 为真就发 `DraftsChanged`，哪怕什么都没变。
- 发信的 4xx、存 Sent 失败按 30 秒起、最多 30 分钟退避，一直重试，没有尽头。

## 决策

- 草稿按退避而不是"只记被拒的版本、不改就不再传"：邮箱腾出空间后用户不碰草稿也该能上去。同一版本连续被拒
  从 5 分钟起翻倍、最多 6 小时；草稿一改、或用户要求上传（`Uploads::All`）立即照常上传。草稿不设上限：
  它一直在本地，不会丢，`upload_error` 一直显示。
- 系统时间往回调：`upload_retry_at` 晚于"现在 + 最大退避"的只能是调时间之前设的，`STALE` 视为已到期
  （`UploadDue`）。发件箱是先把这种时间改写成现在（`bring_forward`）；草稿在查询里判断即可，不用写库。
- 发信和存 Sent 都是第 3 次尝试失败就停（`MAX_ATTEMPTS`），尽早让用户知道。曾建议约一天（48 次），用户
  选择三次。连接失败本来就不计次数，离线不会因此失败。
- 发信转 `failed`（`last_error` 说明原因，用户可重试或改回草稿）。存 Sent 不能用 `failed`（信已发出），新增状态
  `unsaved`：不再自动重试，用户再存一次（→ `accepted`，次数保留以先查 Sent）或放弃副本。考虑过用
  `accepted` + `next_attempt_at IS NULL` 表示，但是隐式的，前端、CLI、MCP 都得各自推断，不如一个明确的状态。
- `failed` 现在也包括多次暂时失败，界面文案从"服务器拒绝"改为中性的 "This message couldn't be sent" /
  "Not sent"。

## 方案

- `0001_init.sql`：`drafts` 加 `upload_failed_revision`、`upload_failures`、`upload_retry_at`；outbox 状态说明加
  `unsaved`。
- `catmail-store/src/drafts.rs`：`STALE` 带可选的 `UploadDue`（现在、最长等待），过滤退避未到的被拒版本；
  `need_upload` / `stale` 加 `due` 参数；`upload_failed` / `upload_refused` 记次数和下次时间；成功时一并清掉（`UPLOAD_OK`）。
- `catmail-core/src/draft_sync.rs`：`Uploads::taken`（一轮开始时算好的 `UploadDue` 和编辑中的草稿）；退避常量；`DraftsChanged` 只在有变化时发。
- `catmail-types` 的 `OutboxStatus::Unsaved`；`catmail-store/src/outbox.rs` 的 `give_up_saving`，`finish` 接受
  `unsaved`；`catmail-core/src/send.rs` 的 `MAX_ATTEMPTS` 和 `next_attempt`（重试时间或放弃，发信和存 Sent 共用）。
- 前端、CLI、MCP 的状态文案；`docs/architecture/sending.md`、`cli.md`。
- 测试：store 的 `a_refused_draft_waits_for_its_backoff_unless_it_changes`；core 的
  `background_sync_uploads_a_refused_draft_again_only_once_it_changes`、`temporary_failures_fail_after_a_few_attempts`、
  `saving_to_sent_gives_up_after_a_few_attempts`、`an_unsaved_copy_can_be_given_up`。去掉修复后四个 core 测试都失败；去掉时钟回调的条件，store 测试失败。
