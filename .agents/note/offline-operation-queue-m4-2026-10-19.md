# M4（一）：离线操作队列——标记、移动、删除

- 日期：2026-10-19
- 类型：功能

## 背景

M3 之前客户端对服务器只读（除了把发出的信存进 Sent）。M4 第一部分让用户能标记已读 /
星标、移动、删除邮件，并且离线可用：改动立刻反映在本地，之后可靠地到达服务器，不会因为
断线、UIDVALIDITY 改变或用户在回放期间继续操作而改错邮件。IDLE 推送与每账户 actor、草稿
同步到服务器的 Drafts 是 M4 后面的两个 PR。

## 决策

- **每封邮件一条 op**（`pending_ops` 改为 `folder_id + uidvalidity + uid` 加 `kind`、
  `flags_add/remove`、`target_folder_id`、`message_id`），而不是一条 op 带一串 UID 的 JSON。
  回滚、冲突、"回放期间用户又改了这封"都按单封处理，回放时再把相邻的同类 op 合并成一条
  命令（最多 500 个 UID）。标记全部已读这类大操作多几行，SQLite 不在乎。
- **移动的邮件立刻出现在目标文件夹**：行直接移过去、`uid` 置空（`messages.uid` 改为可空），
  正文和索引随行保留，离线也能读、还能按原位置下载正文。服务器给 COPYUID 时行得到新 UID、
  `MessageId` 不变；没有就删行、由同步重新拉取。放弃了"只在源文件夹删掉、等同步再出现"的
  简单做法：离线时移到归档的邮件会消失、正文要重新下载。
- **回放期间的并发改动**：回放不加锁挡住用户（本地操作要即时），而是结算时只认"仍是回放时
  那样"的 op，其余指向旧位置的 op 跟着 COPYUID 改指向新位置；移回原处的 op 不删、回放时在本地
  结算，因为它可能正在执行。
- **标记只记录真正改变的位，回放用 `+FLAGS` / `-FLAGS`**，不整体替换，不覆盖别的客户端的修改，
  失败时能精确撤销。同一文件夹先回放标记、再回放移动删除，这样给已在本地移走的邮件改标记
  （记在它服务器上的原位置）能在它离开之前生效。
- **UIDVALIDITY 不一致时回放什么也不做**，交给随后的同步开始新代际、把 op 标为冲突；只回放的
  `replay_pending` 也一样，不在没同步的情况下重建文件夹。
- **删除**：不在 Trash 的移到 Trash，已在 Trash 或 `permanently` 才永久删除；没有 Trash 的账户
  报错而不是悄悄永久删除。没有 UIDPLUS 时用普通 EXPUNGE（会一并清除别人标了 `\Deleted` 的
  邮件，与其他客户端一致）；没有 MOVE 时 COPY + `\Deleted` + EXPUNGE。
- **失败与冲突的 op 保留**给用户看（`catmail ops`），由用户清除；pending 的不能清除。
- **文件夹被删除**时先撤销它的 op 的本地效果（移出的行删除、移入的行回到原处），再随文件夹
  级联删除（M3 遗留的"pending_ops 随文件夹级联删除"）。
- **CLI 当场回放**，与 `catmail send` 一致；连不上时改动保留、命令非零退出并提示
  `catmail ops flush`。后台回放等每账户 actor（下一个 PR）。
- 回复被服务器接受（或用户确认已送达）时给原信加 `\Answered`，走同一个队列。
- 同步把移动过来的邮件第一次拿到 MODSEQ 算作"flags 更新"，Dovecot 测试发现后改为只有 flags
  或关键字变化才计数。

- **codex review 修复**（3 条，均已核实，先写出不修就失败的测试）：
  - 没有 MOVE 的服务器上，COPY 之后、删除之前断线（或 COPY 的答复丢失），重试会再 COPY 一次，
    目标里出现两份。proto 的 `uid_move` 只做 MOVE，退路拆成 `uid_copy` + `uid_delete` 由 core
    编排。第一版在重试时按 Message-ID 去目标里找，复核时又被指出两个问题（都用故障测试复现）：
    目标里早有 Message-ID 相同、内容不同的邮件时，会误以为已复制而删掉原邮件（丢信）；没有
    Message-ID 的邮件照样复制两份。现在改为**检查点**：COPY 前持久记下目标的 UIDVALIDITY 和
    UIDNEXT，重试时只在 UID ≥ 检查点的新邮件里找原文逐字节相同的；找到就只删原处，找不到就
    重新复制，原邮件已不在说明复制早已确认；目标被重建等无法判断时 op 失败、原邮件留在原处、
    不再自动重试。放弃了给原邮件打临时关键字再去目标里找（污染 flags、要清理，且关键字随
    COPY 复制后仍需去重）。
    再次复核又发现：COPY 答复丢失后，用户把邮件移回原处或永久删除，这两种改动不再走副本核对
    （移回原处在本地结算、删除直接执行），目标里留下副本。现在有检查点的 op 在所在文件夹里
    先于一切核对：找到副本就删除原处、op 跟到副本那里（移回变成从目标移回，删除变成删除副本），
    回放随即再处理新增了 op 的文件夹；没找到就清掉检查点照常继续。
  - 连续的标记都被拒绝时（先已读、再未读），逐条撤销会把第一条的乐观状态留下。现在按"待回放
    操作 + 被拒绝的那条"逆推出之前的 flags，再重放其余的并重新记录它们实际改变的位。
  - 新代际把移出的邮件变成冲突、删掉目标文件夹里的行时，只通知了源文件夹，打开着的目标
    视图会留着已删除的邮件；删除文件夹撤销本地移动时同理。现在这些路径返回受影响的文件夹，
    同步提交后通知（`Folders::reconcile` 返回 `Reconciled`）。

- **/simplify 清理**（四个方向的审查）：
  - 同步和 `replay_pending` 共用一个 `ops::replay`。同步在对账完文件夹之后、同步任何文件夹之前
    全部回放一遍，之后只读一次文件夹状态，不再逐个重读；在同一轮里先同步、后被移入邮件的
    文件夹也不会漏掉。同步计数改回触发器维护的 `total_count` 减去无 UID 的行，不再对整个
    文件夹计数。
  - 移动和删除按目标分组回放，交替"归档、删除"不再逐封往返；事件每个文件夹发一次。
  - store：一个 `queue` 写 op，`failed` 用一条语句（`target_folder_id IS ?`），按列名读 kind，
    "移动中的邮件在服务器上的位置"这个 join 只写一处（`MOVING`），Trash 和发送用的 Sent 都用
    `Folders::by_role`，目标文件夹用 `Folders::get`；可改 flags 的检查从 core 移到
    `PendingOps::set_flags`；移回原处由 `moved()` 自己处理；回放的移动 op 必带目标名
    （`ReplayKind`），去掉 core 里伪造的协议错误。
  - `\Answered` 放进 `Outbox::mark_accepted`（只在真正转换时），`mark_sent` 回到 `user_decision`。
  - CLI：`mark` / `rm` 用门面返回的账户，不再逐封读整封邮件；`PendingOp` 带主题，`catmail ops`
    不再逐条查询；文件夹名称表与 `search` 共用。测试辅助函数移进 `tests/common`。
  - 没采纳：把"服务器上的位置"直接存在邮件行上（`server_folder_id`，设计改动太大）；
    `mv` 仍逐封查账户（要按账户解析目标文件夹）；CLI 的 JSON 用 `json!` 而不为此加 serde
    依赖；`Session` 的 `writable` 与 `selected` 合并、各账户并发执行的通用辅助函数、
    `Permit` 包装、假服务器 `change_flag` 复用 `set_flags`（收益小）。

- **第二次 /simplify**（复核修复之后的代码）：
  - 回放结束后哪些文件夹的邮件变了，由 store 的 `applied` / `moved` / `failed` 如实返回，core
    不再按"源文件夹加目标"去猜：之前标记被拒绝、撤销在已移走的邮件上时，目标文件夹收不到通知，
    而只删了 op 的又会无谓地刷新列表。`PendingOpsChanged` 每次回放发一次。
  - 检查点改用 STATUS（一次往返，不用 EXAMINE 目标再 SELECT 回来）；proto 加 `Session::status`、
    `fetch_sizes`、`can_move`，假服务器支持 STATUS。
  - 核对副本时源文件夹被重建不再伪造协议错误让整次同步失败，而是只停下这个文件夹，留给同步。
  - 核对时先比大小，只下载大小相同的候选和原邮件；原邮件已不在的按目标分组结算。
  - 标记分轮排序（`row_number() OVER (PARTITION BY uid)`），交替的标记不再逐封往返。
  - `Settle` 在 UIDVALIDITY 确认后才构造；`ReplayKind::op_kind` 只在测试里；`Flags::apply`；
    `columns::conversion_error` 复用；测试辅助 `messages()` 移进 `tests/common`。
  - 没采纳：重试时把找到的原邮件和新 COPY 的原邮件合在一次删除里（只在重试时省两次往返，
    却要合并两条不同的失败路径）；`Session` 的 `writable` 与 `selected` 合并；CLI 测试辅助合并、
    原文取 Subject 的测试辅助（收益小）。

## 方案

- `catmail-types`：`PendingOp`、`OpKind`、`OpStatus`、`ReplayReport`、`PendingOpId`，事件
  `PendingOpsChanged`，`FolderSyncReport` 加 `ops_applied` / `ops_failed`，`Flags` 加集合运算。
- `catmail-store`：`0001_init.sql`（`messages.uid` 可空、移动计数触发器、新 `pending_ops`）；
  新 repository `PendingOps`（`ops.rs`：本地改动、`pending` / `applied` / `moved` / `failed` /
  `attempted`、冲突与文件夹删除的清理）；`Messages::upsert` 不插回待移动 / 删除的 UID，
  `update_flags` 叠加待回放的标记，`location` 让移动中的邮件指向服务器上的原位置；
  同步计数不含无 UID 的行；`Outbox::mark_accepted` 给原信加 `\Answered`。
- `catmail-proto`：`Session::select`、`uid_store`、`uid_move`（MOVE 或 COPY 退路，返回
  `CopyUid`）、`uid_delete`；修改类命令的答复按宽松方式解析（状态与 COPYUID 才重要），并跟踪
  EXISTS。
- `catmail-core`：`ops.rs`（入口、回放、`replay_pending`），同步在同步各文件夹之前先回放；`MailClient::set_flags` / `move_messages` /
  `delete_messages` / `replay_pending` / `list_pending` / `dismiss_pending`。
- `catmail-testkit`：SELECT 可写，`UID STORE` / `COPY` / `MOVE` / `EXPUNGE`、COPYUID；故障：
  关掉 MOVE、拒绝修改、修改后不答复就断开。
- `catmail-cli`：`catmail mark` / `mv` / `rm` / `ops`（list / flush / dismiss），`sync` 输出回放结果。
- 验证：store 单测 15 个（只记录改变的位、同步不覆盖、失败撤销、移动与 COPYUID、无 COPYUID、
  回放期间继续移动和标记、移回原处、移动失败回原处、删除经 Trash、新代际冲突、文件夹删除、
  跨账户、清除）；proto 3 个（MOVE、无 MOVE 无 UIDPLUS 的退路、拒绝后连接可用）；core
  `tests/ops.rs` 14 个场景（COPY 答复丢失后移回原处或永久删除不留副本；含答复丢失；无 MOVE 时 COPY 答复丢失不重复（无 Message-ID）、
  COPY 前断线且目标有同 Message-ID 的邮件不丢信、目标被重建时保留原邮件；拒绝、UIDVALIDITY 改变及其事件、
  离线下载、`\Answered`）；
  CLI 1 个（回放、未知文件夹不改动、离线保留）；Dovecot 测试里对真实服务器改 flags、MOVE、
  永久删除并检查 maildir，同步后不重复拉取。fmt、clippy、全部测试通过，Dovecot / submission
  真实服务器测试本地通过。
