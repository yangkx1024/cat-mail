# 加固 IMAP 连接、补上丢弃行的标签事件、假服务器支持 LITERAL+/-

- 日期：2026-11-18
- 类型：修复

## 背景

#331 里"以后再做"的四条，都是便宜的加固：

1. `drop_moving_rows` / `drop_unplaced` / `place` 删掉还在路上的行之前不记 `Touched::shown`。在 Gmail 上，
   移动到已删除时没有 COPYUID，或者 UIDVALIDITY 变了，被删的行从它的标签（Important、Starred）列表里
   消失，却没有事件，列表要等下次刷新才对。
2. `MAX_ANSWER` 只数字节，而整个答复解析后都留着：`* 1 EXISTS` 只有 12 字节，解析后是 112 字节，恶意服务器
   能让一条命令占到上限的好几倍。
3. 取消安全只靠调用方：`execute` 的 future 被丢掉后，连接上没有东西拒绝下一条命令，剩下的答复会被当成
   下一条的。现在每个调用点都会丢掉会话，没有 bug。
4. 假服务器从不通告 LITERAL+ / LITERAL-，非同步 literal 的线路形式只有 `encode` 的单元测试覆盖。

## 决策

- 丢弃行在删除之前用 `messages::shown` 查出它出现的标签和 Starred，只有真删了才记进 `touched.shown`。
  `drop_moving_rows` 先查出行再逐行走 `drop_unplaced`，和其他路径共用一处。
- 每条响应按字节再加 128（`RESPONSE_COST`，略大于 `size_of::<Data>()`）。没有改成按条数单独设上限：一个
  预算比两个好推理，大邮件和一堆小响应都按大致的内存算。flags 同步按窗口取，离上限很远。
- `out_of_step` 从"出错时置上"改成"发出命令时置上、读到 tagged 完成时清掉"。这样任何半途放弃（出错、超时、
  future 被丢）都自动覆盖，原先两处手动置位就去掉了。NO / BAD 是读到了完成，连接照常可用。
- 假服务器的 LITERAL+ / LITERAL- 做成严格的：没通告时收到 `{n+}`、或 LITERAL- 下超过 4096 字节，回 BAD 并
  断开（RFC 7888），客户端用错了会直接失败，而不是被默默接受。Gmail 模式默认通告 LITERAL-，和真 Gmail 一样，
  所有 Gmail 测试都走这条路。

## 方案

- `catmail-store/src/ops/replay.rs`：`drop_unplaced` 和 `place` 多一个 `&mut Touched`，`drop_moving_rows` 改成
  先查再删。测试 `dropped_rows_change_the_lists_that_showed_them`（没有 COPYUID、新的 UIDVALIDITY 两种情况）。
- `catmail-proto/src/imap/connection.rs`：`RESPONSE_COST`、`check_answer`；`next_tag` 置 `out_of_step`，`step`
  读到自己的 tagged 完成时清掉。测试：`many_small_responses_count_for_more_than_their_bytes`、
  `a_command_dropped_half_way_leaves_the_connection_unusable`、`a_refused_command_leaves_the_connection_in_step`；
  `the_limit_is_per_command` 的上限按新的计法改了。
- `catmail-testkit/src/imap/`：`Faults::literal_plus` / `literal_minus`、`ServerState::synchronized_literals`；
  `read_command` 返回 `Read`。`catmail-proto/tests/imap_session.rs` 的
  `literals_wait_for_the_server_only_where_it_asks` 在三种能力下各 APPEND 一封小信和一封超过 4 KiB 的信，
  核对等了几次 `+`、存下的字节。
- 新测试在去掉修复后都失败（手动验证过）。文档：protocol.md、labels.md、testing.md。
