# 错误分类（ErrorKind）与 core 会话帮助函数清理

- 日期：2026-10-21
- 类型：重构

## 背景

Issue #28：`catmail-core::Error` 用 `#[error(transparent)]` 直接包裹 `catmail_store::Error` 和两个
proto 错误枚举，下层枚举因此成了公开 API（core 的测试在匹配
`Error::Store(catmail_store::Error::AlreadyExists(_))`）；没有统一的分类，前端只能从
`ServerProblem { error: String, auth_failed: bool }` 和 `FolderSyncReport.error: Option<String>` 的文本
猜图标和动作。M6 的 OAuth 会引入新的用户动作，需要先有分类。

Issue #35：三项机械性清理——打开文件夹并核对 UIDVALIDITY 有五种写法；`with_session` / `on_session`
的 `for<'s> FnMut(&'s mut Session) -> SessionFuture<'s, T>` 迫使每个调用点 `Box::pin` 并在闭包里克隆
捕获；`Err(Error::Imap(e)) if !e.is_fatal()` 手写了多处。

## 决策

- `ErrorKind`（`Network`、`Auth`、`Refused`、`OutOfSync`、`NotFound`、`Invalid`、`Conflict`、
  `Internal`）和 `ReportedError { kind, message }` 放在 `catmail-types`，因为 `ServerProblem`、
  `FolderSyncReport` 在那里。`ServerProblem.auth_failed` 去掉，改为 `error.kind == Auth`，避免两处
  状态不一致。
- 暂不加 issue 里的 `Reauthorize`：现在没有任何地方产生它，等 OAuth（#29/#30）时再加。`ErrorKind`
  不加 `#[non_exhaustive]`，这样加种类时前端的 `match` 会编译失败。
- `Error::Store` / `Imap` / `Smtp` 改包不透明的 `StoreError` / `ImapError` / `SmtpError`（字段仅
  crate 内可见），去掉 `lib.rs` 对 proto 枚举的 re-export；store 的 `AlreadyExists` 提升为
  `Error::AlreadyExists`。
- IMAP / SMTP 错误的种类由 proto 自己的 `imap::Error::kind()` / `smtp::Error::kind()` 给出，和
  `is_fatal` 放在一起，加变体时只需改一个文件；core 只分类自己的变体。
- `is_transient()`：网络类，或 SMTP 非永久的拒绝（复用 `smtp::Error::is_permanent`，即 4xx）。`is_refusal()`：IMAP / SMTP 错误且连接仍可用。`send.rs`
  的 `is_permanent` 改为基于 kind：`Refused | Invalid | Conflict` 且非 transient；本机问题
  （`Internal`）仍按暂时失败重排，与原来行为一致。
- GUI 侧栏按种类显示：`Auth` → "Can't sign in"，`Network` → "Can't reach"，其他 → "Can't sync"
  （原来非登录问题一律显示 "Can't reach"）。
- `with_session` / `on_session` 改为 `impl AsyncFnMut(&mut Session) -> Result<T>`。实测
  （rustc 1.98）非 `move` 的 async 闭包、或 `async move` 闭包里捕获**引用**并跨 `.await` 使用，都会
  触发 "implementation of `Send` is not general enough"（高阶生命周期的编译器限制）；只有
  `async move` 且捕获拥有所有权的值（克隆 `Arc`）才能通过。所以调用点写成
  `{ let engine = engine.clone(); async move |session| … }`，规则写在 `with_session` 的文档里。
  比原来少了 `SessionFuture` 别名、`Box::pin` 和每次调用的克隆；`attempt_save` 读出的邮件直接留在
  闭包状态里供重连后重用，不再需要 `Arc`。
- 打开并比较 UIDVALIDITY 统一为 `MailboxStatus::same_generation(expected)`（proto），四处
  （`sync_folder`、`replay_folder`、`body::examine`、`find_copies` 两处）各一行。最初按 issue 写了
  `open_generation(session, name, expected, Mode) -> Opened`，但它只是包了 `examine` / `select`，调用点
  反而变长，简化时换成了状态上的方法；各处对新代际的反应不同，仍由调用方决定。不比较的两处（`draft_sync::settle`、`send::attempt_save` 的
  Sent 查重）照旧直接 EXAMINE，加注释说明为什么不需要比较。
- 顺手把 `find_copies` 的 5 元组改成具名结构体 `Searched`。`watch::run` 抽 `Backoff` 没做：这次没改
  到那段逻辑。
- proto 层的 `!e.is_fatal()`（`Settle::outcome`、`copy_move`、`find_copies`、`upload_one` 对 proto
  `Result` 的判断）保持不变，那是 proto 错误自己的方法。

## 方案

- `crates/catmail-types/src/error.rs`（新）；`account.rs`、`sync.rs` 字段改为 `ReportedError`。
- `crates/catmail-core/src/error.rs`：不透明包装、`kind()`、`is_transient()`、`is_refusal()`、
  `From<&Error> for ReportedError`，加单元测试。`engine.rs` 的 `report` 改存 `ReportedError`。
- `catmail-proto`：`imap::Error::kind`、`smtp::Error::kind`、`MailboxStatus::same_generation`。
- `connection.rs`：`AsyncFnMut` 签名；`sync.rs`、`ops.rs`、`body.rs`、
  `watch.rs`、`send.rs`、`draft_sync.rs` 改调用点。
- 测试改为按 kind 断言（`tests/sync.rs`、`send.rs`、`watch.rs`、`submission.rs`，`client.rs` 的重复
  账户测试）；GUI `sidebar.rs`、`mailbox/mod.rs` 跟着改。
- 文档：`docs/architecture.md` 新增"错误"一节，`docs/architecture/sync.md` 更新账户状态与文件夹
  错误的描述。
- 验证：`cargo fmt`、`clippy -D warnings`、`cargo test --workspace` 通过；本地起 Dovecot + mailpit
  跑了 `--test dovecot --test submission`，全部通过。
