# Store 拆成 1 写 + 读池

- 日期：2026-10-21
- 类型：重构

## 背景

issue #34。`Store` 是 `Arc<Mutex<Connection>>`，`list_messages` 等 UI 读要排在同步每批 200 条插入
（每条还带一次 FTS `reindex`）的写事务后面。两边都在 blocking pool 上，不冻结 UI，但同步期间列表明显
延迟。WAL 早已开启，`Store::read` / `Store::write` 的 API 允许不动 core 地拆开。

## 决策

- `write` / `write_durable` 仍走唯一的写连接；`read` 从只读连接池借连接，WAL 下读到开始时最后一次
  提交的快照，不等进行中的写。
- 池自己写（Mutex + Condvar，按需打开、最多 4 个，满了等归还），不引入 r2d2：需求只有借还和上限。
- 只读连接用 `SQLITE_OPEN_READ_ONLY` 而不是 `query_only`：`read` 闭包里误写直接报错，也不会去抢写锁。
  只在迁移之后打开，不会看到旧 schema。
- 内存数据库另开连接是另一个空库，读继续用写连接（只有测试用）。考虑过 shared cache 的内存库，但它
  不支持 WAL，读写仍然互斥，没有意义。
- `Inner` 里读池先于写连接 drop：最后关闭的连接负责 checkpoint 并删除 WAL，只读连接做不到。
- "闭包不得回调同一个 `Store`"的规则不变，AGENTS.md 里补上原因：写里套写死锁、写里套读看不到未提交
  的改动、读里套读可能耗尽池。

## 方案

- `crates/catmail-store/src/lib.rs`：`Store { inner: Arc<Inner> }`，`Inner { readers: Option<Readers>,
  writer: Mutex<Connection> }`；`Readers::get` 借出 `Reader`（drop 时归还并唤醒等待者）。
- 测试：`reads_do_not_wait_for_a_write`（写事务进行中，另一线程的读 5 秒内返回且看到写之前的快照；
  改回走写连接时该测试失败）、`reads_cannot_write`、`more_concurrent_reads_than_connections`（3 倍于
  上限的线程并发读写，结束后连接数不超上限且全部归还）。
- 文档：`docs/architecture/storage.md`、`docs/architecture.md`、`docs/architecture/sending.md`、AGENTS.md。
