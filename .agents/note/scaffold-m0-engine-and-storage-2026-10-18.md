# M0：搭建 workspace、存储层与账户管理

- 日期：2026-10-18
- 类型：功能

## 背景

项目从零开始。目标是一个跨平台的 Rust 邮件客户端：核心引擎与 UI 无关，可以直接包一层 CLI
在终端使用，GUI 基于 GPUI + gpui-kit。M0 需要定下分层、数据模型和引擎骨架，让 M1 的 IMAP
同步可以直接在上面实现。期间根据 codex 的两轮代码 review 和一轮设计 review 做了修正。

## 决策

- **分层**：`types ← store, proto ← core ← cli / gui`，前端只依赖 `catmail-core` 的
  `MailClient`。GUI crate 推迟到 M5，避免现在就引入 GPUI 的编译开销。
- **IMAP 库选 `imap-next`**（用户选择）。`imap-codec 2.0.0-alpha.9` 漏开 `nom/alloc`，在
  `catmail-proto` 里显式依赖 `nom` 绕过。
- **运行时桥接**：`MailClient` 自建或借用 tokio runtime，公开方法 await `JoinHandle`，
  因此可以被 GPUI 的 executor 直接 await。
- **邮件按"文件夹里的一封邮件"建模**：一行对应一个 IMAP（folder, UID），flags 各自独立。
  放弃了最初"一封邮件 + 多个位置"的模型，它无法表达不同文件夹副本的不同 flags；也不用
  Message-ID 合并副本。
- **凭据**：密码只放系统 keyring，键为 `account:{email}:{随机 uuid}`。添加时先写密钥再插入行，
  删除时 `DELETE … RETURNING` 后再删密钥，从顺序上消除孤立凭据。放弃了"数据库级 store_id
  + 进程内锁"的方案，它覆盖不了多个实例。
- **每个数据目录一个引擎**：用文件锁保证。放弃了多进程同时写的方案（需要任务领取、租约、
  跨进程通知），代价是 GUI 运行时 CLI 不能打开同一目录，之后用 IPC 解决。
- **事务**：所有数据库访问经过 `Store::read` / `Store::write`，先提交再发事件，事件在引擎
  任务内部发出，调用方 drop future 也不会丢。
- **为后续里程碑预留的表结构**：`pending_ops` 绑定 folder + UIDVALIDITY；outbox 存完整
  SMTP 信封并细分状态（`accepted` 只重试 APPEND，`unknown` 不自动重发）；跨账户引用用复合
  外键。
- **迁移**：项目未发布，表结构直接改 `0001_init.sql`，不保留兼容。

## 方案

- `crates/catmail-types`：账户、文件夹、邮件摘要、`Flags`、`Event`、ID 类型；`BlobHash`
  反序列化时也做校验，防止路径穿越。
- `crates/catmail-store`：`migrations/0001_init.sql`；`Store` 的 `read` / `write` 事务接口；
  `Accounts` repository；内容寻址 `BlobStore`（重复写入刷新 mtime，配合 GC 宽限期）。
- `crates/catmail-core`：`MailClient` builder、运行时桥接、`SecretStore`（keyring / 内存
  实现）、数据目录锁、账户增删与事件。
- `crates/catmail-cli`：`catmail account add | list | remove`，支持 `--json`、`--data-dir`、
  `--password-stdin`。
- `flake.nix` 提供开发环境；`docs/architecture.md` 记录架构设计。

验证：24 个单元测试（含并发添加/删除、drop future 仍发事件、跨账户外键、事务回滚、
数据目录锁等），`clippy -D warnings` 通过；用真实 Secret Service 端到端验证了凭据的写入
和删除；用 `flock` 占锁验证了跨进程的数据目录锁。
