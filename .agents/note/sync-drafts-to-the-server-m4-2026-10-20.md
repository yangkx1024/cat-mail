# M4（三）：草稿同步到服务器的 Drafts 文件夹

- 日期：2026-10-20
- 类型：功能

## 背景

M3 的草稿只在本地。换一台设备或用别的客户端时看不到正在写的信，本机数据丢了草稿也就没了。把草稿
放进服务器的"草稿箱"需要每次修改都换一份副本、删除旧的，所以等到 M4 有了操作队列再做。这是 M4 的
最后一部分。

## 决策

- **按状态对齐，不按操作入队。** 草稿的 `revision` 与副本的版本（`remote_revision`）不同就上传，
  而不是每次保存都入队一条"上传"：连续多次修改只上传最后一版，离线期间改了多少次都一样。
- **先上传新的，再删旧的。** 服务器上始终至少有一份。不要的副本经现有操作队列永久删除
  （`PendingOps::delete_on_server`），复用 UIDVALIDITY 核对、离线排队、失败与冲突的处理；但只按服务器
  刚报告的代际下的 UID 入队，其余先按 Message-ID 找到（见下文 codex review 修复）。用户已经移走或删除
  那封邮件时不再入队（一封邮件最多一条移动或删除）。
- **APPEND 答复丢失不留两份。** 与保存到 Sent 同一思路，但这里每次上传都用新的 Message-ID，在 APPEND
  之前用 `write_durable` 记进 `draft_uploads`；没结算的上传先于任何新上传按 Message-ID 搜索结算。
  没有 UIDPLUS 时同样靠搜索拿 UID。考虑过整个草稿用固定的 Message-ID、按最大 UID 认定最新副本：
  APPEND 其实没做成时会把旧副本当成新版本，放弃。
- **上传期间草稿被删除或发出**：`draft_uploads.draft_id` 用 `ON DELETE SET NULL`，结算时把找到的副本
  删掉，删除草稿的代码不必特殊处理。比草稿现有副本旧的上传只删除它找到的东西，结算顺序不影响结果。
- **UIDVALIDITY 改变时找回副本。** 第一版直接忘掉副本、重新上传，但服务器多半只是重新编号，旧副本
  还在，会多出一份。现在记下副本的 Message-ID（`remote_message_id`），新代际时把副本转成一条待结算的
  上传，按 Message-ID 找回、记下新 UID，不重新上传。
- **草稿副本保留 Bcc 头**（`Outgoing::bcc`，只对草稿传入），否则其他客户端打开草稿会丢掉密送；发信
  的构建不变。flags 为 `\Draft \Seen`。
- **时机**：与回放共用 `ops::replay_queued`（同步、`replay_pending`、后台同步三处都调用它），上传在
  回放之前，旧副本在同一次回放里删除。草稿的每次修改都唤醒后台任务。`replay_pending` 在既没有待回放的
  op、也没有要上传的草稿时不连接服务器。
- **被拒绝的上传**（超出配额等）记在 `drafts.upload_error`，`Draft` 暴露 `on_server` 与
  `upload_error`，`ReplayReport` 加 `drafts_uploaded` / `drafts_failed`；下一轮再试。附件 blob 缺失
  导致构建失败也记录，不挡住其他草稿和操作的回放。
- **CLI 当场上传，失败只警告。** `compose` / `reply` / `draft edit|attach|detach|rm` 和 `send` 之后
  调用 `replay_pending`。与 `mark` 不同，连不上时以零状态退出、只在 stderr 警告：草稿本身已经存好，
  非零退出会让脚本以为写信失败。`ops flush` 仍以非零状态报告被拒绝的草稿。`draft list` 加 SERVER 列，
  `draft show` 显示未上传或被拒绝的原因。
- **只单向上传。** 其他客户端写的草稿在 Drafts 里是普通邮件，不导入为本地草稿；放到以后（需要解析
  MIME 回填草稿、处理两边同时修改）。GUI 写信（M5）自动保存频繁，那时再加去抖。
- `remove_draft_attachment` 改为返回草稿（CLI 要知道账户），与 `update_draft` 一致。
- Dovecot 测试用单独的 `drafts` 用户：和对预置邮件的同步测试共用 `catmail` 用户时，并行运行会让后者的
  "再同步一次没有变化"断言失败（实测发现）。

- **/simplify 清理**（四个方向的审查）：
  - store：`delete_on_server` 与 `queue` 共用一个写 `pending_ops` 的 `insert`；`need_upload` 用
    `Folders::by_role` 找 Drafts，不在 SQL 里再写一遍角色；`begin_upload` 用调用方已有的值构造结果，
    不再回查刚插入的行；新代际时副本的处理从 `folders.rs` 移到 `Drafts::end_generation`，与
    `PendingOps::end_generation` 并列。
  - core：回放和上传共用一个不唤醒的 `ops::replayed` 发事件，不再手抄 `announce`（用 `announce` 会
    唤醒后台任务，多同步一次 INBOX）；`upload_one` 返回 `Outcome` 枚举，不再是嵌套的
    `Result<Option<Result<…>>>`；"结算，服务器拒绝时留到下一轮"合成 `try_settle`；去掉 `Done`。
  - CLI：`draft` 命令只在分派之后保存一次；"被拒绝的草稿"和"下一次 sync / ops flush"的提示与
    `ops flush` 共用。
  - 没采纳：编辑去抖（属于 M5 的 GUI 自动保存，已记为已知问题）；没有 UIDPLUS 时按文件夹合并搜索、
    一轮的 `begin_upload` 合成一次落盘（少见路径，或要先构建出所有邮件）；`on_server` 与
    `upload_error` 合成一个枚举（JSON 输出要变，收益小）；`Replayed` 改成元组、同步报告里加草稿
    计数；`remove_draft_attachment` 改回不返回草稿（CLI 要知道账户）；去掉 `upload` 里与
    `replay_account` 重复的 `need_upload` 检查（同步和后台每次都会走到，这次读省掉后面的读）；与
    `attempt_save` 共用"EXAMINE 后按 Message-ID 搜索"（两行）；把上传结果存成 Drafts 里的邮件行、
    经 `delete_messages` 删除（要在结算时写入邮件头）。
- **codex review 修复**（3 条，均已核实，先写出不修就失败的测试）：
  - APPEND 在发出邮件之前被拒绝（Dovecot 无法写入时不回 `+`）时 proto 返回致命的 `Error::Aborted`，
    上传原样把它往上传，而上传在同步任何文件夹之前：账户的每次同步都在这里失败，再也收不到新邮件。
    现在把它当成拒绝记进 `upload_error`，再换一条新连接继续。假服务器的 `reject_append_literal` 复现。
  - `Error::Incomplete` 不是拒绝：tagged OK 之后有一行解析不了也返回它，邮件其实已经存下。之前按拒绝
    删掉了上传记录，下一轮再上传一份，第一份没人管。现在只有 NO/BAD 和提前拒绝算拒绝，其余不致命的
    错误按 Message-ID 结算。假服务器加了 `garble_append`。
  - 文件夹被重建（UIDVALIDITY 改变）、还没有同步发现时修改草稿：APPENDUID 报告新代际，旧副本却按本地
    记下的旧代际入队删除，同步之后变成冲突，而它的 Message-ID 已被覆盖，副本永远留在服务器上。同一
    窗口里删除草稿也一样。根本原因是按一个可能过时的代际使用 UID，所以改为只按服务器刚报告的
    UIDVALIDITY（APPENDUID，或结算时的 EXAMINE）下的 UID 删除；其余情况（删除或发出的草稿、另一代际
    或另一文件夹里被替换的副本）都把副本转成一条没有草稿的上传，下一轮按 Message-ID 找到再删除。
    `Outbox::finish` / `cancel` 因此不再需要返回受影响的文件夹，恢复原来的签名。代价是删除草稿时多一次
    EXAMINE 和 SEARCH。

## 方案

- `catmail-store`：`0001_init.sql` 的 `drafts` 加 `remote_message_id`、`remote_revision`、
  `upload_error`（CHECK 覆盖五个 `remote_*` 列），新表 `draft_uploads`；`Drafts` 新增 `need_upload`、
  `stale`、`uploads`、`begin_upload`、`uploaded`、`upload_refused`、`upload_failed`、`end_generation`，
  `delete` / `remove` 把副本交给下一轮按 Message-ID 删除；`PendingOps::delete_on_server`；
  `Outbox::finish` 经 `Drafts::remove` 删除草稿。
- `catmail-proto`：`Outgoing::bcc`。
- `catmail-core`：新模块 `draft_sync.rs`（`upload`、`settle`）；`ops::replay_queued` 取代三处各自的
  回放；`drafts::build` / `new_message_id` 与发信共用；草稿修改唤醒后台同步。
- `catmail-cli`：草稿命令当场上传、`draft list` / `show` 显示服务器状态、`ops flush` 报告草稿。
- `catmail-testkit`：假 IMAP 的 `garble_append` 故障。
- `testenv`：Dovecot 加 `\Drafts` 邮箱和 `drafts` 用户。
- 验证：store 单测 9 个（替换副本并删旧的、同步过的行一并删除、删除的草稿按 Message-ID 删副本、
  草稿已删时删除上传的副本、旧上传不顶替新副本、版本变了或在发送中不上传、拒绝后记录并重试、没有
  Drafts 文件夹、新代际找回副本、另一代际的旧副本按 Message-ID 删除、发出后删除副本、
  `delete_on_server` 不碰用户已移走的邮件）；proto 1 个（草稿保留 Bcc 且不能注入头）；core
  `tests/drafts.rs` 11 个场景（每次修改替换副本、删除与发出、APPEND 答复丢失（有无 UIDPLUS）只留一份、
  答复丢失后草稿被删、被拒绝后重试、提前拒绝不挡住同步、答复不完整仍结算、没有 Drafts 文件夹与未同步、
  Drafts 被重建只留一份、重建后同步前修改或删除不留副本、后台同步立即上传）。确认去掉"先结算"时两个答复
  丢失的测试失败、去掉新代际找回时重建测试多出一份、去掉唤醒时后台测试超时，三条 codex 修复各自去掉时
  对应的测试失败。Dovecot 测试对真实服务器上传、替换、删除并检查 maildir。手动用 CLI 对 Dovecot 写信、修改、
  同步，停掉服务器后修改（警告、零退出、`ops flush` 非零）。fmt、clippy、全部测试通过，Dovecot /
  submission 真实服务器测试本地通过。
