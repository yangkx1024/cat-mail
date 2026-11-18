# MCP 改动日志给结果加引号并截短留下的文字

- 日期：2026-11-17
- 类型：修复

## 背景

审计 #331：`MCP change` 那一行用 `%outcome` 记结果，而工具的错误会复述 agent 给的文字（比如
`invalid: the content of {name} isn't base64`）。tracing-subscriber 的输出不转义换行，文件名里带一个换行就能
伪造出一行日志。`logged()` 留下的 `from`、`to`、`cc`、`bcc`、`name` 也没有长度上限，而一个请求可以有 16 MB。

## 决策

结果改用 `?outcome`（Debug，带引号并转义控制字符），与参数的 JSON 一致；不自己写转义。留下的字符串和结果各截到
200 字，截过的以 `…` 结尾。

## 方案

`catmail-mcp` 的 `tools/mod.rs` 加 `capped`，`logged()` 和 `call_tool` 用它；`tests/drafts.rs` 加测试（长地址被截、
换行不进日志行）。`docs/architecture/mcp.md` 的“改动的日志”写明。
