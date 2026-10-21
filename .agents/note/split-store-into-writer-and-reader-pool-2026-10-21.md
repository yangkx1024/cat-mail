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
- 池自己写（`Mutex<Vec<Connection>>` + Condvar，打开 `Store` 时一次开好 4 个，都借出时等归还），不
  引入 r2d2：需求只有借还和上限。起初按需打开，要多记一个计数、处理打开失败时的回滚；一次开好代码更短，
  路径或权限问题也在 `Store::open` 就暴露。
- 只读连接用 `SQLITE_OPEN_READ_ONLY` 而不是 `query_only`：`read` 闭包里误写直接报错，也不会去抢写锁。
  只在迁移之后打开，不会看到旧 schema。
- 去掉 `Store::open_in_memory`：内存数据库另开连接是另一个空库，只能让读走写连接，于是 store 的单元
  测试从不经过读池（`read` 闭包里误写在测试里也发现不了）。它只有测试在用，改为 `test_util::store()`：
  临时目录里的文件，`TestStore` 持有 `TempDir` 并 deref 到 `Store`。考虑过 shared cache 的内存库，但它
  不支持 WAL，读写仍然互斥。
- 快照在 `read` 闭包的第一条查询时取得（`DEFERRED`），不是进入 `read` 时：PR 上 Codex 指出原文档
  "运行期间的提交看不到"过强。闭包内的查询仍看到同一份状态、且不早于调用，没有调用方依赖更强的保证，
  所以收紧文档而不是每次读多发一条查询。
- `Inner` 里读池先于写连接 drop：最后关闭的连接负责 checkpoint 并删除 WAL，只读连接做不到。
- "闭包不得回调同一个 `Store`"的规则不变，AGENTS.md 里补上原因：写里套写死锁、写里套读看不到未提交
  的改动、读里套读可能耗尽池。

## 方案

- `crates/catmail-store/src/lib.rs`：`Store { inner: Arc<Inner> }`，`Inner { readers: Readers,
  writer: Mutex<Connection> }`；`Readers::get` 借出 `Reader`（drop 时归还并唤醒等待者）。
- 测试：`reads_do_not_wait_for_a_write`（写事务进行中读 5 秒内返回且看到写之前的快照；改回走写连接时
  该测试失败）、`reads_cannot_write`、`more_concurrent_reads_than_connections`（3 倍于连接数的线程
  并发读，其中一个同时在写，结束后连接全部归还）。store 的其余单元测试也都改用文件，经过读池。
- 文档：`docs/architecture/storage.md`、`docs/architecture.md`、`docs/architecture/sending.md`、AGENTS.md。
