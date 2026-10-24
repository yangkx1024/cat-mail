# 修复 Gmail 标签副本在搜索里重复、过时副本打不开

- 日期：2026-10-24
- 类型：修复

## 背景

Gmail 账户删除一封邮件后搜索它，出现两条结果：一条在"已删除邮件"里，能打开；另一条在"重要"里，打开
报错 "the message is no longer on the server; sync the account and try again"。

Gmail 的文件夹是标签，一封邮件同时在 INBOX、"所有邮件"、"重要"里，本地各是一行。移到"已删除邮件"时
Gmail 去掉了它所有的标签，但"重要"不是 INBOX，没有 IDLE，本地副本要等下一次完整同步（15 分钟）或打开
那个文件夹才删掉；搜索跨所有文件夹，所以搜到它，打开时按旧 UID 去取就失败。没删除时，同一封邮件在搜索
里也会出现两三次（INBOX + 所有邮件 + 重要）。

## 决策

- 搜索按 (账户, Message-ID) 合并，只用于有 `\All` 邮箱的账户。AGENTS.md 规定邮件按文件夹存、不按
  Message-ID 合并，这里不改存储，只改搜索显示哪一行；没有 `\All` 的服务器上不同文件夹里的副本是独立
  邮件，照旧。想过用 Gmail 的 `X-GM-MSGID` 精确认同一封，但 imap-codec 2.0.0-alpha.9 解析不了。
- 想过只在搜索里排除"所有邮件"等虚拟文件夹，但只归档过的邮件只在"所有邮件"里，会搜不到。
- 回放移动后向服务器问其他标签里那几封副本还在不在（`UID SEARCH UID`），不在才删，而不是本地按
  Message-ID 直接删：Message-ID 只用来挑要问的 UID，不会误删 Message-ID 相同的别的邮件。最初的做法是
  同步整个副本所在文件夹，但移到 Trash 后 EXISTS 对不上，会对"所有邮件"做完整 UID 对比，代价与文件夹
  大小成正比，而且要在三处回放的调用点各加一次；改为在 `replay_queued` 里只问这几封。
- 打开时发现邮件已不在，先 `UID SEARCH UID` 确认再删本地行，答复里只缺某个部分时不删。

## 方案

- `Messages::search`（`catmail-store/src/messages.rs`）：`row_number()` 按 (账户, Message-ID) 取一行，
  `SEARCH_PICK` 决定优先级（本地刚移入的行 > INBOX / 已发送 / 草稿 / 归档 / 标签 / 垃圾 / 已删除 >
  已加星标 / 所有邮件），在游标和 LIMIT 之前取，翻页稳定。在真实数据库上 8 千条匹配约 12 ms。
- `LABEL_ACCOUNTS`（`catmail-store/src/folders.rs`）：哪些账户算标签账户，搜索和找副本共用。
- `PendingOps::label_copies`（`catmail-store/src/ops.rs`）在回放之前找出副本的文件夹和 UID，记进引擎
  每账户的内存状态（`Engine::add_label_copies`，按文件夹合并 UID）；`replay_queued` 回放之后逐个文件夹用
  `sync::drop_vanished` 问服务器、删掉不在的，问过才从引擎里去掉。不放在 `Replayed` 里：后台同步和打开文件夹每次换连接重来都新建一个，
  移动做成后断线，副本就再也找不出来了。新增 `Session::uid_search_uids`，以及 fake server 的
  `drop_on_open` 故障。
- `body.rs` 的 `on_server`：取邮件的操作返回 `Option`，`None` 表示服务器什么也没给；再用
  `uid_search_uids` 确认没有这个 UID，才用 `Messages::delete_gone` 删本地行、发 `MessagesChanged`。
  `MessageLocation` 新增 `server_folder`（服务器上那个文件夹），删除时按它比对：下载与回放并行，查位置
  之后移动才做成、目标里 UID 和 UIDVALIDITY 碰巧与源相同时，按显示的文件夹比对会删掉有效的行。
- AGENTS.md 的 "Messages are per mailbox" 写明了标签账户这个例外。
- 文档：`storage.md`、`op-queue.md`、`bodies.md`、`sync.md`。
- 测试：store 单元测试 `search_shows_label_copies_once`、`deletes_a_message_gone_from_the_server_only_if_unchanged`、
  `moves_on_a_labels_account_name_the_other_copies`；fake server 测试
  `moving_a_message_drops_its_other_gmail_labels`、`moving_a_message_leaves_its_copies_elsewhere_alone`
  （ops）、`a_message_moved_out_leaves_its_other_gmail_labels_at_once`、
  `other_gmail_labels_are_looked_at_after_a_drop_past_the_move`（watch，断线发生在移动做成之后），以及
  `refuses_uids_from_an_old_uidvalidity_or_expunged_messages`（bodies）新增的断言。去掉相应修复后 ops、watch 的
  测试和 store 里移动做成之后的那条断言都失败。
