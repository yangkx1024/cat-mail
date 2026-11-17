# 把 client.rs 里的账户逻辑和 builder 拆出去

- 日期：2026-11-17
- 类型：重构

## 背景

issue #320 第 1 条：`catmail-core/src/client.rs` 约 2.8k 行，是唯一把引擎逻辑写在门面里的文件。
账户和身份的增删改、登录检查、密钥顺序都写在 `MailClient` 的方法体里，`MailClientBuilder` 和打开引擎的
代码也在同一个文件；测试混着测发件箱、builder 和账户。只读方法有两种写法：一种
`rt.run_blocking(|| engine.store.read(..))`，一种 `rt.run(async { engine.read(..).await })`，后者多起一个
任务只为了再 `spawn_blocking`。

## 决策

- 新建 `accounts.rs`，照 `ops::` / `send::` 的样子放接 `Arc<Engine>` 的自由函数：`add_account`、
  `add_identity`、`remove_account`（异步，门面用 `rt.run`），`update_identity`、`remove_identity`（阻塞，
  门面用 `rt.run_blocking`，和原来一样不多起任务，同 `gc::collect`）。`check_account`、`check_identity`、
  `check_smtp`、`insert_with_secret`、`forget_secret`、`validate_*`、`check_password`、`server`、
  `spawn_background` 都跟过去。`Verify` 也搬到这里（`lib.rs` 改从 `accounts` 导出，公开路径不变）：
  引擎一侧的模块不该反过来依赖门面。
- 输入检查（`clean_name`、`validate_*`、`check_password`）仍在调用方线程、取引擎之前做
  （`check_new_account` / `check_new_identity`），所以引擎已关闭时无效输入还是先报 `InvalidInput`，
  和原来一致。
- `client.rs` 保留文件名，加 `client/builder.rs`（`MailClientBuilder`、`Opened`、`open_engine`、
  `MailClient::builder` / `builder_locked` / `start`，以及交接目录的 `clear_handoff_dir`、
  `remove_old_files`、`KEEP_HANDED_OFF`）。没有改成 `client/mod.rs`：仓库里 `catmail-frontend` 用的就是
  `foo.rs` + `foo/` 的写法，而且保留文件名 `git blame` 好追。`Shared` 和 `MailClient` 的字段仍私有，
  子模块本来就看得见。
- 只读门面统一成一个私有的 `MailClient::read(|db| ..)`：`rt.run_blocking(move || Ok(engine.store.read(f)?))`。
  选 `run_blocking` 而不是 `run(async { engine.read })`：两者都在引擎运行时的阻塞池里读，结果和错误
  转换一样（`StoreError` 经 `?` 转成 `Error`，任务被取消是 `Error::Closed`），前者少起一个异步任务。
  `MailClient` 的 21 个只读方法和 `DraftEditor::images` 都用它（后者把 blob 路径的拼接放进读的闭包里，引擎的引用仍留在运行时
  那边，不进调用方的 future）。`sync_now` 读完还要逐个账户触发同步，不是纯读，没动。
- 测试跟着代码走：账户、身份、密钥顺序的 7 个去 `accounts.rs`；发件箱的 5 个去 `send.rs`；打开、锁、
  数据目录权限、交接文件、后台任务随客户端结束的 9 个去 `builder.rs`；真正测门面的 7 个（`closed`、
  运行时桥接、调用方 future 被丢掉后事件仍发出、`DraftEditor::send` 等）留在 `client.rs`。共用的夹具
  （`open_client`、`client_with_account`、`new_account`、`draft_to_b`、`queue_one`、`secret_key`、`stored`、
  `GatedSecrets`）放进 `#[cfg(test)] client/fixtures.rs`，各处引用，不重复；只一处用的
  `FailingSecrets`、`LockableSecrets` 跟着各自的测试。

## 方案

- 新增 `crates/catmail-core/src/accounts.rs`、`src/client/builder.rs`、`src/client/fixtures.rs`；
  `client.rs` 从约 2.8k 行减到约 1.3k 行；`lib.rs` 加 `mod accounts`，`Verify` 从 `accounts` 导出；
  `send.rs` 收下发件箱测试。`docs/architecture/storage.md` 的凭据一节注明密钥顺序在 `accounts.rs`。
- 纯搬移，不改行为；`MailClient` 和 `lib.rs` 导出的公开 API 不变。
- 验证：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo test -p catmail-core` 和依赖它的 `catmail-cli`、`catmail-mcp`、`catmail-frontend` 的测试通过；
  `catmail-core` 单元测试前后都是 77 个，测试名逐个对上，一个没少。
