# 收紧数据目录内部文件的权限

- 日期：2026-10-20
- 类型：修复

## 背景

issue #23。`create_data_dir` 只在新建数据目录时设 0700，里面的东西都是 umask 默认权限：`catmail.db`
及 `-wal` / `-shm` 是 0644，`blobs/`、`blobs/tmp`、`handoff/` 是 0755。复现：`--data-dir` /
`CATMAIL_DATA_DIR` 指向一个已存在的 0755 目录，整个邮件库（邮件头、正文、草稿）对其他用户可读。另外对
已存在的路径只检查 `is_dir()`，共享位置上被人预先放置的符号链接会被静默使用。删除的正文还会留在
SQLite 空闲页里。

## 决策

- 不依赖外层目录：数据库文件 0600，内部目录 0700，旧版本留下的在打开时去掉 group / other 位。数据库
  文件先以 0600 创建再交给 SQLite，而不是创建后 chmod：中间打开它的读者会一直能读。SQLite 新建的 WAL /
  shm 沿用主文件的 mode，上次留下的单独收紧。
- 已存在的数据目录本身不改权限（可能是用户指定的家目录之类），宽于 0700 时只 warn。
- 符号链接：没有一律拒绝（把数据目录链接到别的盘是正常用法），而是要求目录（跟随链接后）属于当前
  用户，否则 `Error::DataDirNotOwned`。别人放的链接指向的是他自己的目录，这样也被挡住。std 没有安全的
  `geteuid`（workspace 禁止 unsafe），引入 `rustix`（已在依赖树里）。
- `handoff/` 改由引擎在打开时清空后重建为 0700，GUI 不再自己 `create_dir_all`。
- `secure_delete=ON` 而不是 `FAST`：`FAST` 不清零进入空闲列表的页，大段正文在溢出页里，正是要清的。
  代价是被释放的页都要写一遍（经 WAL），包括 FTS 合并释放的；考虑过只在删除账户、UIDVALIDITY 重建时
  打开，但同步中的单封删除、GC 也会留下正文，暂时全局打开，写放大成问题时再收窄。
- Windows 上不处理 ACL，继承父目录的。

## 方案

- `catmail-store/src/private.rs`：`create_private_dir`（core 也用它建数据目录和 `handoff/`），内部的
  `make_private` / `new_file`。
  `Store::open` 用它们处理 db / wal / shm 并开 `secure_delete`；`BlobStore::open` 和 `put` 的前缀目录用
  0700。
- `catmail-core/src/client.rs`：`check_data_dir_owner`（属主检查 + warn），打开时重建 `handoff/`；
  `error.rs` 新增 `DataDirNotOwned`。
- 测试：`new_file` 不截断已有文件；`database_files_are_private`、`deleted_content_is_overwritten`、
  `blobs_are_private`；core 的 `files_in_a_shared_data_dir_are_private`、
  `data_dir_of_another_user_is_refused`（链接到 `/`，root 下跳过）。
- 文档：`architecture.md` 数据目录一节、`architecture/storage.md`、`architecture/gui.md`。
