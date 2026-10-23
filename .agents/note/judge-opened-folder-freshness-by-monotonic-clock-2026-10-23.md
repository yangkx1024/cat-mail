# 打开文件夹的同步用单调时钟判断新鲜度

- 日期：2026-10-23
- 类型：修复

## 背景

Issue #55。`sync_one`（`MailClient::sync_folder`，GUI 打开文件夹时调用）用墙上时钟的
`last_synced_at` 判断是否在 `max_age` 内同步过。系统时间往回调之后，之前同步过的文件夹的
`last_synced_at` 留在"未来"，`now() - t` 为负、永远小于 `max_age`，时钟追上来之前每次打开都不同步。
复现：同步后把 `last_synced_at` 加一天，服务器上来一封新邮件，`sync_folder` 返回 `None`、拿不到它。

## 决策

- Issue 给了两个方案：改用引擎内存里的 `Engine::folder_synced`（`Instant`），或保留 `last_synced_at`
  但把"未来"的时间当作没同步过。选了前者：完整同步跳过插进来的同步做完的文件夹时已经用它（#54），
  两处用同一个时钟，"这个文件夹多久前同步过"只有一个来源。
- 代价是记录只在内存里：重启之后每个文件夹第一次打开都会同步一次，直到启动时的后台完整同步做完它。
  打开时多同步一次是可以接受的，后者则可能整天不同步。
- `last_synced_at` 留在库里：CLI 的 JSON 输出还用它显示上次同步时间。

## 方案

- `crates/catmail-core/src/sync.rs`：`sync_one` 的新鲜度判断改为
  `engine.folder_synced(account, folder).is_some_and(|t| t.elapsed() < max_age)`。只拉部分 flags
  的 INBOX 同步本来就不调用 `set_folder_synced`，与不更新 `last_synced_at` 的行为一致。新鲜度检查
  挪到读文件夹之前：它只查内存，刚同步过的文件夹（最常见的情况）不必再读一次数据库。
- `engine.rs` 的 `folders_synced` 字段注释、`docs/architecture/sync.md` "打开的文件夹"一节同步更新。
- 测试：`tests/sync.rs` 的 `a_clock_set_back_doesnt_make_an_opened_folder_look_fresh`，把 Archive 的
  `last_synced_at` 改到一天后、服务器上加一封新邮件，`sync_folder(archive, Duration::ZERO)` 必须同步
  并拿到它。改动前该测试失败。
