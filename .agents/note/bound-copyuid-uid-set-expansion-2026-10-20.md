# 限制 COPYUID 的 UID 区间展开

- 日期：2026-10-20
- 类型：修复

## 背景

Issue #21。`copy_uid` 把 `[COPYUID uidvalidity src dst]` 的两个 UID 集合先各自展开成
`Vec<u32>` 再比较长度，区间端点完全由服务器决定。回答 `COPYUID 1 1:4294967295 1:4294967295`
的服务器会让客户端分配两个 16 GiB 的向量，分配失败即 abort 进程，GUI 随引擎一起退出。
`UID MOVE` / `UID COPY` 的应答都会走到这里（操作队列回放、COPY 兜底、草稿副本删除）。

## 决策

- 上限取本次请求的 UID 数（`copy_or_move` 传 `uids.len()`）：COPYUID 的源集合只能是请求的
  子集，超过即视为服务器乱答，按"没有 COPYUID"处理，调用方已有这条退路（之后靠同步找回副本）。
  没有另设常量上限：请求数本身就是精确且足够小的界。
- 先把集合转成区间，用 u64 求和计数，两边相等且不超过请求数才展开；展开用
  `flatten + zip`，不再先物化两个独立向量。

## 方案

- `crates/catmail-proto/src/imap/session.rs`：`copy_uid` 增加 `requested` 参数，先计数后展开。
- `crates/catmail-testkit/src/imap.rs`：新增故障 `forged_copyuid`，让 COPY / MOVE 应答带上伪造的
  COPYUID。
- `crates/catmail-proto/tests/imap_session.rs`：`ignores_a_copyuid_naming_more_uids_than_were_moved`
  覆盖超大区间、超出请求数的集合（去掉上限检查时该断言失败），以及反向区间的正常配对。
