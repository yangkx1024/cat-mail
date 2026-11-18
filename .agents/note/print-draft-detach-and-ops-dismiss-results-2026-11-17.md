# `draft detach`、`ops dismiss` 有输出，文档写明哪些命令在 `--json` 下不输出

- 日期：2026-11-17
- 类型：修复

## 背景

审计 #331：cli.md 说每次输出是一个带 `schema_version` 的对象，但 `draft detach` 两种模式都什么也不输出（引擎已经
返回了改后的草稿），`ops dismiss` 也不输出；`draft rm`、`identity default|name|rm|saves-sent`、`mcp token rm`
在 `--json` 下不输出。

## 决策

`draft detach` 与 `draft edit` 一样：`--json` 输出改后的草稿，文本模式一行确认。`ops dismiss` 的文本模式按
`draft rm`、`identity rm` 的样子每个一行确认，`--json` 下不输出。其余只做改动的命令不另造输出结构：cli.md 写明它们
在 `--json` 下什么也不输出、成败看退出码（没有现成的小结构可以照搬）。

## 方案

`catmail-cli` 的 `cmd/compose.rs`、`cmd/ops.rs`；`docs/architecture/cli.md` 的 `--json` 一节。命令的标准输出没有
测试能抓到，靠 clippy 与现有测试验证。
