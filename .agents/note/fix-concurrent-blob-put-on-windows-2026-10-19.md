# 修复：Windows 上并发写入同一 blob 失败

- 日期：2026-10-19
- 类型：修复

## 背景

M2 合并后 main 的 CI 在 Windows 上失败：`bodies.rs::concurrent_downloads_store_one_body`
报 `Io(Os { code: 5, kind: PermissionDenied, "Access is denied." })`，Linux、macOS 通过。

两个并发下载同一封邮件，会同时把相同内容写入 `BlobStore`。`put` 里两个线程都发现目标
不存在，然后都 `persist`（Windows 上是 `MoveFileExW(REPLACE_EXISTING)`）。覆盖一个正被
另一线程覆盖或打开的文件，Windows 返回 `ACCESS_DENIED`；Unix 的 rename 是原子替换，不会
出错。所以只在 Windows 上偶发。

## 决策

- 改用 `persist_noclobber`，从不覆盖已有 blob：内容寻址，目标存在就说明是同一内容，覆盖
  本来就没有意义。失败后若目标已经是文件，视为别人已存好，刷新 mtime（GC 宽限期）后返回。
  不覆盖也就不会有"正在被替换"的文件，`touch` 打开目标时同样不会撞上。
- 考虑过遇到 `PermissionDenied` 时重试 `persist`，放弃：仍然会替换文件，只是把竞争窗口
  缩小，且要猜重试次数。

## 方案

- `crates/catmail-store/src/blob.rs`：`put` 用 `persist_noclobber`，目标已存在时 `touch`
  后返回；类型文档注明从不覆盖。
- `docs/architecture.md`（附件 / BlobStore）说明原因。
- 新测试 `blob::tests::concurrent_puts_of_the_same_content_all_succeed`：8 个线程用
  `Barrier` 对齐后同时写入相同内容，重复 50 轮。Linux 上 rename 原子替换，这个测试在修复前
  也能通过，本地无法复现；它和原来失败的 `concurrent_downloads_store_one_body` 在 Windows
  CI 上验证修复。
