# 支持 IMAP MESSAGELIMIT（RFC 9738）

- 日期：2026-11-19
- 类型：功能

## 背景

RFC 9738 允许服务器限制一条命令处理的邮件数（#239）：SEARCH、FETCH、STORE、MOVE（UID 形式）和
UID EXPUNGE 从最大的 UID 往下做 N 封就停，照样回 OK，只多一个 `[MESSAGELIMIT N LastUID]`；COPY 超过 N
封整条拒绝。原来这个码按名字记下后没人看，截断的结果被当成完整的：同步的删除检测（`UID SEARCH ALL`
求差）会把更早的本地邮件当成服务器上已经删了，`fetch_flags` 漏掉的变化在 HIGHESTMODSEQ 前进后永远
丢失，批量 STORE / MOVE 只做了一部分就删掉 `pending_ops`。接过的服务器都没见过这个码，属于预防。

## 决策

- 在 `Session` 里续完，调用方（sync、ops）拿到的总是完整的结果，不用各自处理。
- SEARCH 续发用 `UID 1:<LastUID-1>` 而不是 RFC 新加的 `UIDBEFORE`：前者所有服务器都认。
- 改动类命令统一走 `change_uids`：按声明的上限预先分段（从最大的 UID 往下，和服务器的顺序一致），
  被 `NO [MESSAGELIMIT]` 拒绝时按上限重分，部分完成时对 `LastUID` 以下的再发。MOVE / UID EXPUNGE
  也截到 `LastUID` 以下再发，而不是按 RFC 的说法原样重发：这样每轮集合都变小，进度可以检查。
- 没给 `LastUID` 或 `LastUID` 没有下降时不再续（防死循环），返回新的 `Error::Truncated`，不复用
  `Incomplete`（那是"答复解析不了"）。原样重发 STORE 只会重做同样的 N 封，所以没给 `LastUID` 也不重试。
- COPY 分段后"被拒绝就什么都没复制"不再成立：后面一段被拒时返回 `Truncated`。没有 MOVE 的移动把它
  当成答复丢失（记一次尝试，op 和检查点留着），下次回放用现有的 checkpoint 核对找出已复制的，
  不新加状态。当成拒绝的话，已复制的那些会在目标里多出一份。
- imap-proto 的 pin 升到 25f47f4（含 e27fa0c 的 `ResponseCode::MessageLimit`），不再自己按名字解析。

## 方案

- `catmail-proto`：`connection.rs` 把 tagged NO 上的码变成 `Error::MessageLimit`，严格的 `execute` 把带码的 OK 当成 `Truncated`，加 `Response::merge` / `message_limit`；
  `session.rs` 的 `execute_below`（`uid_search`，以及经 `uid_fetch_all` 的 `fetch_flags`、`fetch_summaries`、
  `fetch_previews`、`fetch_sizes`）、`change_uids`（STORE、Gmail 标签、MOVE、COPY、UID EXPUNGE）、
  `copy_uids` 合并各段的 COPYUID；IDLE 后按序号的 FETCH 被截断时当 `ChangedFlags::Unknown`；
  `Capabilities::message_limit` 读 `MESSAGELIMIT=` / `SAVELIMIT=`；`error.rs` 加 `MessageLimit`、
  `Truncated`（都不致命）。
- `catmail-core/src/ops.rs`：`copy_move` 遇到 `Truncated` 记尝试、保留检查点。
- `catmail-testkit`：`Faults::message_limit`（以及 `hide_message_limit`、`message_limit_without_uid`、
  `refuse_copies_after`）。
- 测试：`catmail-core/tests/sync.rs` 的 `a_server_s_limit_on_messages_per_command_loses_nothing`（去掉
  修复后删除检测删掉 7 封旧邮件、标志变化丢失）；`tests/ops.rs` 的批量标志 / 移动（MOVE 与 COPY 两种，
  声明与不声明上限）和 `a_move_copied_in_part_is_finished_without_copying_twice`（去掉 core 的改动后 5 个
  op 被标为失败）；`catmail-proto/tests/imap_session.rs` 检查续发 / 分段的命令和没给 `LastUID` 的情况。
- 文档：`docs/architecture/protocol.md`（连接层）、`op-queue.md`（没有 MOVE 的服务器）。
