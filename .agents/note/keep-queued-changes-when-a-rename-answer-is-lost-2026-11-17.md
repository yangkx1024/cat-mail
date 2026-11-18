# 重命名的答复丢失时保留待回放的操作

- 日期：2026-11-17
- 类型：修复

## 背景

Issue #331。`folders::rename` 在新建的连接上发 `RENAME` 后连接断开或超时（服务器已经改了名，答复没到），
错误直接返回，`Folders::rename` 没有执行，本地仍是旧名字。下次同步对账时旧名字不在服务器上，`remove()`
删掉这个文件夹，`pending_ops` 按 `folder_id` / `target_folder_id` 级联删除：还没回放的标记、删除、移出
悄悄丢了，删掉的邮件又回来。复用的连接已经有 `pool::on_session` 的重来（先 `LIST` 判断做没做成），
新建的连接没有。

## 决策

- 在 `pool.rs` 里加 `with_session_redone`：操作跑过、连接断开（`is_disconnected`）时，不只复用的连接，
  新建的连接也在新连接上再来一次，告诉操作这是重来，由已有的 `LIST` 检查决定是否再发命令，并照常更新本地
  记录。`with_session` / `on_session` 行为不变（共用同一个 `trying`，只多一个开关）：它们被同步、正文、回放等
  共用，那些地方新连接失败后再试一次只会把超时翻倍，而它们的失败本来就会在下次重来。最初写在 `folders.rs`
  的外层重来（`on_server`）要克隆闭包、用 `AtomicBool` 推断"跑过没有"，重来时还会从池里拿另一条可能也死了
  的连接、离线时多等一次连接超时；放进连接池的循环里这些都不需要。
- 新建、删除走同一个函数，顺带也不再把做成了的操作报成失败；删除的重来本来就按服务器现在的层级删。
- 还没连上就失败的（离线）不重来：命令没发出去，再连一次只多等一次超时。
- 再来一次也失败（网络整个断了）时问题仍在；要彻底解决得记下做到一半的改名，留待以后。

## 方案

- `crates/catmail-core/src/pool.rs`：`with_session_redone`，与 `with_session` / `on_session` 共用 `trying`。
- `crates/catmail-core/src/error.rs`：`Error::is_disconnected`。
- `crates/catmail-core/src/folders.rs`：三个操作改用 `with_session_redone`，`listed_again` 并入调用处。
- `docs/architecture/folders.md`：更新"答复丢了不当成拒绝"与测试一节。
- 测试：`tests/folders.rs` 的 `a_rename_whose_answer_was_lost_on_a_new_connection_keeps_queued_changes`，
  `resume()` 让池里的连接作废、重命名走新连接，`drop_after_mailbox_change` 让答复丢失；检查重命名成功、
  `RENAME` 只发了一次、同步后文件夹与邮件都在、排队的加星到了服务器。改动前该测试失败。
