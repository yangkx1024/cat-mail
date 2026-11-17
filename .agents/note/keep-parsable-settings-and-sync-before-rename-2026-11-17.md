# settings.json 只丢解析不了的值，保存时留着不认识的，覆盖前留副本，写入先 fsync

- 日期：2026-11-17
- 类型：修复

## 背景

#326：`SettingsFile::load` 整体反序列化 `settings.json`，一个值解析不了（手改出来的 `"appearance": "auto"`、
更新的版本写的值、越界的端口）就整个返回默认值；下一次改设置把默认值整份写回，主题、MCP 开关和端口、保持运行
都丢了。`write_atomically` 在 rename 前不 fsync，断电可能留下空文件，走同一条路。读不了的文件（权限）也会被
rename 覆盖掉，因为 rename 只要目录的写权限。

## 决策

- issue 给了两条路：改名成 `.bad`，或者逐字段解析。两个都做：逐值解析让常见的情况（一个坏值）只丢那一个；
  副本保住被丢的值，覆盖前就在，用户能找回来。
- 逐值解析从空对象开始往里放，每放一个就整体试解析一次，放不进去的拿掉。从空对象开始，丢掉的值取的是
  `#[serde(default)]` 给的默认（`keep_running` 是 true），不是类型的默认（`bool` 的 false）。对象整体放不进去时
  逐项放，所以 `mcp.port` 坏了不连带 `mcp.enabled`。只靠 `Settings` 的反序列化判断，不用按字段写代码，以后加
  设置不用改这里。
- 用复制而不是改名：逐值解析之后原文件大部分还有效，不必先让它消失；复制失败就不写回（同读不了时一样）。
- 读不了（NotFound 以外的错误）的文件，这次运行不写回，而不是覆盖它。代价是这次运行里改的设置不保存，日志里有警告。
- 同一类问题的另一种情况：新版本加的设置，旧版本读时忽略，保存时只写自己认识的字段，就丢了（不算解析不了，
  也不留副本）。一起修：`load` 记下文件的对象，保存时把 `Settings` 合并进去（对象逐项合并，其余替换），不认识的
  键在任何一层都留着。考虑过 `#[serde(flatten)] extra: Map<String, Value>`：`McpSettings` 也得加，它就不再是
  `Copy`、`Eq`，用到它的地方都要改，以后加嵌套设置还得记着加；合并不用动类型。代价是 serde_json 没开
  `preserve_order`，写出的键按字母排，不再按字段顺序。

## 方案

- `crates/catmail-frontend/src/settings.rs`：`load` 改用 `parse`（先整体解析，失败再 `keep_parsing` 逐值放），
  返回被丢的值的 JSON pointer；有丢的就把原文写到 `settings.json.bad` 并警告；`read_only` 标记让
  `write_pending` 不写。
- 同一文件：`read` 存 `load` 读到的对象，`save` 用 `merge` 把设置合并进它再写。
- `crates/catmail-frontend/src/fs.rs`：`write_atomically` 在 persist 前 `sync_all`，Unix 上 rename 后再 sync 目录
  （Windows 没有目录句柄可 sync）；主题和开机启动项也用它。
- `docs/architecture/gui.md` 补了这几条。
- 测试：`a_value_that_does_not_parse_leaves_the_others`（坏的 appearance、null、越界端口、未知字段；默认取
  设置的；副本在且保存后不变）、`dropped_values_are_named`、`a_file_that_cannot_be_read_is_not_written_over`
  （Unix，权限 000，root 跳过）、`a_save_keeps_what_the_app_does_not_know`（顶层和 `mcp` 里不认识的键保存后还在），并把空文件、`[]` 加进原有的坏文件测试。前两个改动去掉后新测试失败。
