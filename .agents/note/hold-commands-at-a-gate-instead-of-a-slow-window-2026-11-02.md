# 测试用闸门挡住命令，不再靠固定的慢窗口

- 日期：2026-11-02
- 类型：修复

## 背景

Issue #205。`tests/folders.rs` 的 `while_slow` 给假服务器设 `faults.slow = (<命令>, 300ms)`，
默认测试在这 300ms 里做完后面的检查。CI 机器一卡超过 300ms，DELETE 已经回复、文件夹行已删，
`messages_carrying_a_label_being_deleted_still_change` 就拿到 `FolderNotFound` 而不是 `Conflict`
（PR #204 的 Linux CI 出现过一次）。在检查前插一秒 sleep 模拟卡顿即可稳定复现。

## 决策

- testkit 加按命令前缀的闸门 `Faults::gate: Option<(String, Arc<Semaphore>)>` 和
  `ServerState::hold(prefix)`：命令先记日志，再等许可才回答。已经在等的命令拿着自己那份
  `Arc`，把 `gate` 清掉也不会放行它。
- 原来只给 LOGOUT 用的 `logout_gate` 是它的特例，删掉，`prefetch.rs` 改用 `hold("LOGOUT")`。
  `fetch_gate` / `body_fetch_gate` 按 FETCH 的种类挡、还计数，不是前缀能表达的，保留。
- 其余用 `faults.slow` 的测试逐个看过：
  - `watch.rs` 的两个 LOGIN 测试要在登录被拒之前 `sync_now` / `create_draft`，改用闸门。前者超时会失败，
    后者超时只是变成白测。
  - `sync.rs` 的两个测试等的是另一个任务排进账户同步锁，外面观察不到，换闸门也只能再等一段固定时间，
    去不掉计时；`sync_folder_goes_in_between...` 超时会失败但窗口有 500ms，`a_full_sync_doesnt_skip...`
    超时只会白测。保持不变。
  - `drafts.rs:820` 只是要同步比草稿的延迟慢，卡顿只会更慢，保持不变。
- RENAME 测试里 `create_folder` 要等 rename 持有的同步锁，不能在放行前 await；改成 spawn 后等 100ms
  再放行。这段时间不够只会让它在 rename 之后才开始：测试照样通过，但没测到要测的情形。去掉这类等待
  （和 `sync.rs` 的两个）要能从外面看到有调用在等账户同步锁，那是引擎层的改动，见 #209。

## 方案

- `crates/catmail-testkit/src/imap.rs`：`Faults::gate`、`ServerState::hold`，删 `logout_gate`。
- `crates/catmail-core/tests/folders.rs`：`while_slow` 改为 `while_held`，返回 `Held`，检查做完再
  `finish()` 放行并等调用结束；三个测试改用它。
- `crates/catmail-core/tests/watch.rs`、`prefetch.rs`：改用 `hold`。
- 验证：在两个 DELETE 测试的窗口里插 1 秒 sleep，旧代码在 `folders.rs:397` 失败（与 CI 相同），
  新代码通过；全部测试、clippy 通过。
