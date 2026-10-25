# Gmail 按三个位置同步

- 日期：2026-10-24
- 类型：功能

## 背景

[位置与标签](../../docs/architecture/labels.md)迁移步骤 4 的后半（#127）：#126 准备好了存储和协议，同步仍把
Gmail 的每个标签当文件夹，同一封邮件在 INBOX、"所有邮件"、"重要"里各存一行。改为只同步"所有邮件"、
已删除、垃圾邮件三个位置，一封邮件（`X-GM-MSGID`）一行，标签随邮件存下。

## 决策

- **模式在完整同步对账时定**（`X-GM-EXT-1` 且 LIST 里有 `\All`），与对账在同一个事务里写入；变了就结束
  每个位置的 UIDVALIDITY 代（`Folders::end_generations`）。已删除、垃圾邮件在两种模式下都是位置，也要
  重拉，否则旧行没有 `X-GM-MSGID`，被移出时认不出。
- **Gmail 上不按名称猜角色**：已删除在 IMAP 里隐藏时，名为 Trash 的用户标签会被猜成已删除，进而被当成位置。
- **消失的邮件留到三个位置都同步完才删**（`Unit`；普通账户上每个位置是一个单元，删除走同一条路径），新出现的邮件 `X-GM-MSGID` 已有一行就把那一行
  搬过来（`Messages::upsert` 的 `moved`）。完整同步仍按位置拿锁，不整个单元持有：首次同步"所有邮件"可能
  要几分钟，其间回放和打开文件夹不该一直等。代价是删除前要核对 UIDVALIDITY 仍是记下时那一代。中途断线时
  不删，下次靠条数对不上重新求差集。
- **删除前再看一遍先同步的位置**：评审（codex）指出，邮件在"所有邮件"同步之后、已删除同步之前从已删除
  移回"所有邮件"时，已删除记下它消失，却没有哪个位置看到它出现，结束时这一行（连同正文）被删。先改为删除前
  把较早同步的位置再看一遍，复查又指出这一遍里新消失的（复查"所有邮件"之后从垃圾邮件移回"所有邮件"）
  照样被直接删除（有复现测试）。又改为复查整遍同步三个位置，新消失的再看一遍；/simplify 指出整遍同步
  在消失的那个位置上因为条数对不上会做完整的 `UID SEARCH ALL`，而判断"去了哪里"只需要看新到的。最终：
  每一遍只拉各位置游标之后新到的，移过去的行随之搬走，直到某一遍哪里也没有新到的（最多三遍），这时还在
  原处的才删除；没有新到的就说明它们哪里都不在（`finish`、`arrivals`）。考虑过对每封消失的邮件在其他位置
  `UID SEARCH X-GM-MSGID`，但清空已删除时是几百次往返，而且逐个位置搜索同样躲不过搜索之间的移动。同一次评审指出拉邮件头不完整时也会
  误删，最初还写着"某个位置被拒绝也删，免得它一直失败时消失的行永远留着"；改为任何一个位置不完整就
  一个也不删，宁可留着过时的行。
- **正在本地移动的行**（`uid` 为空）不搬：在移动来源里看到的是它本身（跳过），在别处看到说明服务器已经
  移动而操作还没了结，算 `held`，这一批不完整、不写游标，等回放了结再同步。否则插入会撞上 `gm_msgid`
  的唯一索引。
- **标签的事件**：GUI 和 `catmail watch` 只在 `MessagesChanged` 指向正在看的文件夹时刷新，而 Gmail 的
  INBOX 现在是标签，只发位置的事件，INBOX 就不再实时更新。考虑过让前端把位置的事件当成同账户所有标签
  的变化（GUI 要知道文件夹的 kind，CLI 只会打印"所有邮件"的计数），也考虑过每次变化对所有标签发事件
  （CLI 每次打印几十行）；最后由存储记下自己的写入改变了哪些标签的列表（`Messages::touched_labels`：
  插入、搬行、flags 变化、删除前后显示这封邮件的标签，带 `\Flagged` 的加上"已加星标"，标签增减用
  `RETURNING`），只对它们发。最初在核心里每次写入前后各查一次整批邮件的标签，评审（/simplify）指出
  没有 CONDSTORE 时一个 5000 封的窗口里只变了一封也会对窗口里所有邮件的标签发事件，而且每条写入路径都
  要记得查两次。
- **本地改动也通知标签**（codex 评审）：INBOX 成了标签之后，标记已读只通知"所有邮件"，INBOX 的列表不
  刷新，之后的同步看到 flags 已经一样，也不会再发。`PendingOps` 的标记、移动、删除、拒绝后的撤销都把
  显示这封邮件的标签（与 `Messages` 共用 `messages::shown`）记进 `Touched`。flags 变化只在变化前查一次，
  `\Flagged` 前后有一个就加上"已加星标"；删除按批一次查询；插入时标签由 `set_labels` 的 `RETURNING`
  记下，只有带 `\Flagged` 的再查一次，普通账户上不为每封邮件多查。
- **隐藏过的系统标签重新出现**（codex 评审）：系统标签只能按角色对应到邮箱，邮箱被隐藏时这些标签被
  丢掉；重新显示不会让 MODSEQ 变大，`CHANGEDSINCE` 拿不回来。对账时新出现（或角色变成）INBOX、已发送、
  草稿、重要的标签邮箱，就清空各个位置的 HIGHESTMODSEQ，下次全量拉 flags 和标签。
- **IDLE 唤醒后**同步三个位置再 EXAMINE INBOX；IDLE 报告的 UID 是 INBOX 标签邮箱里的，在位置里没有意义，
  flags 按 CONDSTORE 全部刷新。
- **搜索**：邮件只有一行，去掉按 Message-ID 合并（`SEARCH_PICK`，原定在 #128 删，但留着它会把 Gmail 上
  Message-ID 相同的不同邮件合成一条）；普通账户不搜 `all` / `flagged` 角色的文件夹。`LABEL_ACCOUNTS`
  只剩回放找副本在用，随 #128 删除。
- 没做、留给 #128：草稿邮箱成了标签之后，删除旧草稿副本的操作按本地文件夹的 UIDVALIDITY 核对，标签
  没有这个值，这些操作一直待回放（写进了 `labels.md` 的"草稿与已发送"）；Gmail 不 APPEND 到已发送。
  "所有邮件"被隐藏时提示用户是 #129 的界面。

## 方案

- 存储：`Messages::upsert` 返回 `Upserted { added, moved, moved_from, held }`，按 `X-GM-MSGID` 搬行；
  `touched_labels`（`update_flags`、`set_labels` 改为按行 id）、`note_folder_shown`、`stored`、`delete_uids_of`（核对
  UIDVALIDITY）；`search` 改为 `SEARCHED` 条件；`Folders::location_ids` / `sync_states` 只列位置（"所有
  邮件"排在 INBOX 之后）；`Folders::end_generations`。
- 核心（`sync.rs`）：`mode_of`、`roles` / `remote_folders`、`Unit`（Gmail 与否、推迟的删除）、
  `sync_locations`、`report_on`、`emit_changed`；`run`、`sync_one`、`sync_inbox` 按模式选单元。`send.rs`
  找 Sent 时只取角色（`roles`）。
- 假服务器：`ServerState::move_message`（另一个客户端在位置之间移动）。
- 文档：`labels.md`（状态、"同步"一节改成现状、草稿的未完成项、迁移步骤）、`sync.md`、`storage.md`
  （搜索）、`op-queue.md`、`testing.md`、`architecture.md`、AGENTS.md。
- 验证：新增 `tests/gmail.rs`（只 EXAMINE 三个位置、标签视图与计数、移到已删除再移回同一行带正文、
  "所有邮件"同步后断线不删、只改标签随 CHANGEDSINCE 拉到并通知 INBOX、隐藏 / 显示"所有邮件"切换模式且
  待回放操作成为 `conflict`、IDLE 唤醒后同步三个位置、打开标签、同步途中、复查途中移回"所有邮件"仍是同一行、本地改动通知标签、隐藏过的
  "重要"重新显示后有邮件）；store 测试
  `search_finds_each_message_once_where_it_is`、`a_row_moving_locally_stays_until_the_move_is_replayed`，
  `labels_show_and_count_what_is_in_all_mail` 改为断言搬行。分别去掉推迟删除、标签事件、模式变化时的重建，
  对应的测试都失败。`cargo test --workspace`、clippy、Dovecot 与 submission 真实服务器测试通过。CI 里没有
  真的 Gmail，2026-10-25 用测试账户手动验证：首次同步、移到已删除再移回（同一行、正文保留）、只改标签、
  后台同步、隐藏再显示"所有邮件"切换模式，结果记在 `labels.md` 的"验证结果"。
