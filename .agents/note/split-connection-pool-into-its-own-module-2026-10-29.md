# 连接池拆成独立的 pool 模块，并加上日志

- 日期：2026-10-29
- 类型：重构

## 背景

issue #158：#156 加了 keepalive 之后，连接池分在两处：存储和保活在 `engine.rs`，取用规则
（`with_session` / `on_session`）在 `connection.rs`。"睡前建立的连接不用"和"只有致命错误才不放回"各
写了两遍；keepalive 任务靠每账户递增的 id 认领连接，连接被取走后任务还要空等到点才退出。

## 决策

- 新建 `pool.rs`，把池子、keepalive、`with_session` / `on_session` 都放进去。`connection.rs` 只剩建立
  连接和登录（IMAP、SMTP）。`AccountState` 只挂一个 `Arc<Pool>`，引擎通过 `Engine::pool` 交出。
- 账户删除不再靠"先置 removed 标记、再取池锁清空"这一跨两把锁的约定：池子自己记 `closed`，
  `Pool::close` 和放回都在池锁内完成。
- keepalive 用 `AbortHandle` 代替 id：取走连接就 abort 它的任务，删除账户时一并 abort；任务用
  `tokio::task::id()` 和 `AbortHandle::id()` 认出自己看管的连接，省掉了计数器。任务在池锁内 spawn，
  第一次来找时连接一定已经在池里。
- 顺带把 proto `Session` 的 `selected` 和 `writable` 合成 `Option<Opened { name, status, writable }>`：
  两者总是一起设、一起清。
- 加了连接池的日志，方便以后分析"为什么又登录了一次"。按频率分级：keepalive 发现连接断开、丢掉睡前
  建立的连接、删除账户时关掉连接记 info（少见，默认进日志文件）；取用（闲置多久、连接年龄、池里还剩几条）、
  放回、放不下、每次 keepalive 的 NOOP 往返记 debug：每条池连接每 4 分钟一次，放在 info 会淹没日志，
  需要时用 `CATMAIL_LOG` 打开。`on_session` 换掉睡前连接从 debug 提到 info。
- `Pool` 知道自己的账户，取、放、关都是它的方法，日志也都在里面。core 的 `Error` 加了 `imap()`，
  `with_session` / `on_session` 不再各自手拆 `Error::Imap(ImapError(e))`。
- 顺带改了 #156 的一处：keepalive 放回连接时原来压到最上面，下次取到的是刚 NOOP 过的、而不是最近用过的
  那条，可能多一次 EXAMINE。现在池子按"上次被请求用完的时刻"（`used`）排序，keepalive 保留这个时刻。
- 除此和日志外行为不变。排序的改动有单元测试（`pool::tests`），去掉修复会失败。

## 方案

- `catmail-core/src/pool.rs`：`Pool`（`take`、`take_own`、`close`），`take` / `put`，共用的
  `usable`（`since_woke`）和 `survives`（非致命错误），`keep_alive`，`with_session`、`on_session`。
- `engine.rs` 去掉 `IdleSession`、`IDLE_SESSIONS_PER_ACCOUNT`、`keep_alive`、`take_session` /
  `return_session` / `take_idle`；`start_removal` 调 `Pool::close`。
- 调用方（`body`、`ops`、`send`、`sync`、`watch`、`client`）改用 `pool::`。
- `catmail-proto/src/imap/session.rs`：`Opened`。
- 文档：`protocol.md` 连接池一节、`sync.md`、`logging.md`。
- 验证：fmt、clippy、全部测试通过；`bodies.rs`（含 keepalive 测试，依赖 `take_own` 按 task id 找到连接）
  连跑 20 次通过。
