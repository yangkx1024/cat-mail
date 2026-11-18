# read_attachment 的说明先写服务的话，文件名加引号放后面

- 日期：2026-11-17
- 类型：修复

## 背景

审计 #331：`read_attachment` 的说明行是 `{name} ({mime}, 大小). Its content is the sender's: ...`，寄件人写的文件名
打头又不加引号，一个精心起的文件名就能冒充服务对 agent 说话。

## 决策

服务自己的话（大小、是否截断、内容是寄件人的）固定在前，文件名和 MIME 类型放在后面，用 `serde_json::to_string`
加 JSON 引号（类型也来自寄件人）。没有文件名时写 `none`。

## 方案

`catmail-mcp` 的 `tools/reading.rs` 的 `attachment_contents`；`tests/drafts.rs` 的断言改为检查开头和结尾的引号；
`docs/architecture/mcp.md` 的“附件以内容传出”写明。
