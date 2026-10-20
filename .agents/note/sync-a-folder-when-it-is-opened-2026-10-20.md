# 打开文件夹时同步它

- 日期：2026-10-20
- 类型：功能

## 背景

后台同步一个账户只有一条连接，只能在 INBOX 上 IDLE；其他文件夹靠每 15 分钟一次的完整同步。QQ 邮箱
（不支持 NOTIFY、CONDSTORE）上把 IMAP 收取范围改成"全部"之后，INBOX 很快出现了历史邮件，其他文件夹
要等到下一次完整同步，用户只能手动点 Sync。

## 决策

- 用户打开文件夹时同步这一个文件夹，这是其他邮件客户端的常见做法；没有采用缩短完整同步间隔（流量
  更大，仍然要等几分钟）。
- 新增引擎接口 `MailClient::sync_folder(folder, max_age)`，而不是让 GUI 调 `sync_account`：只同步用户
  在看的文件夹，打开时更快。与完整同步一样先回放操作队列（草稿只上传停止编辑够久的，`Uploads::Settled`），
  否则同步会看到改动之前的服务器。
- "最近同步过就跳过"放在引擎里、拿到同步锁之后检查 `last_synced_at`：GUI 手里的文件夹信息可能已经过时
  （没有变化的同步不发事件），而且等着的完整同步刚同步过它时不必再来一遍。GUI 用一分钟。
- 只有真的连了服务器的同步才计入账户状态（`Engine::report`）。评审指出最初的写法连"刚同步过、跳过"
  也记成成功，会清掉侧栏的连接问题；若是登录被拒，还会唤醒后台同步、绕过它的重试间隔。测试
  `a_skipped_folder_sync_leaves_the_account_status_alone`（改回原写法时失败）。
- 失败只记日志：连不上服务器已经由侧栏底部的账户状态说明，每点一次文件夹弹一次通知太吵。

## 方案

- `crates/catmail-core/src/sync.rs`：`sync_one`，持有同步锁，在连接池的连接上（`with_session`，失效时换新
  连接重来）回放后同步该文件夹；服务器拒绝该文件夹时记在 `FolderSyncReport.error`。结果计入账户状态
  （`Engine::report`）。新增 `Error::FolderNotFound`。
- `crates/catmail-gui/src/mailbox/mod.rs`：`select_place` 选中文件夹时在后台调用 `sync_folder`，阈值
  `SYNC_ON_OPEN_AFTER`（60 秒），测试可以改成 0。
- 测试：`catmail-core/tests/sync.rs` 新增 `sync_folder_brings_one_folder_up_to_date`（只同步该文件夹、
  先回放改动、刚同步过时不连服务器、未知文件夹报错）和 `sync_folder_reports_a_folder_the_server_refuses`；
  GUI 新增 `opening_a_folder_syncs_it`（去掉调用时失败）。
- 文档：`docs/architecture/sync.md` 新增"打开的文件夹"一节，`gui.md`、README 同步说明。
