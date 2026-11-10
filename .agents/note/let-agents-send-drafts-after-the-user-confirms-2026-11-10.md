# 让 agent 经确认后发信

- 日期：2026-11-10
- 类型：功能

## 背景

MCP 的第 5 步：草稿层级的 agent 只能写草稿，发信要用户回到 GUI 或 CLI。设计里写的是"客户端支持 elicitation 时
经它请用户确认，不支持时只有显式允许不确认的令牌能发"。服务端不建会话（MCP 2026-07-28），调用中途不能向客户端
发请求，确认要走无状态的多轮请求。

## 决策

- 两个新层级，接在草稿之后：`send`（先问）与 `send_unasked`（不问）。不另加布尔字段：不问就发是严格更大的权限，
  层级的顺序照样成立，CLI 的 `--tier`、GUI 的菜单都不用改结构。`send_unasked` 无论客户端能不能问都不问，给独自
  工作的 agent。
- 问用 rmcp 3.9 的多轮请求（SEP-2322）：第一次答 `input_required` 带一个没有字段的表单 elicitation，客户端带答复和
  `requestState` 再调。`requestState` 不可信，只放随机句柄，问过的东西（令牌、草稿、revision、时间）记在内存里，
  15 分钟、只用一次、必须同一令牌与草稿；对不上就再问，不报错。放弃在 `requestState` 里放签名的状态：要管密钥、
  防重放，重启后多问一次的代价更小。
- 确认的就是发出去的：引擎加 `preview_send` 与 `send_previewed(id, DraftRevision)`，后者在排进发件箱的写事务里查
  revision 没变。`DraftRevision` 不透明。
- 发送也归编辑器锁管：`send_draft` 和改草稿一样经 `not_open_elsewhere`，GUI 开着草稿时别处发不了；编辑器自己发经
  新的 `DraftEditor::send`。没有另做一套"agent 发送才查编辑器"的判断。
- 客户端问不了（没有 elicitation 表单或协议早于 2026-07-28）时，`send` 的令牌得 `cannot_ask`。

## 方案

- `catmail-core`：`send.rs` 的 `SendPreview`、`DraftRevision`、`preview`，`queue_draft` 多了 `editor` 与 `previewed`
  参数；`MailClient::preview_send`、`send_previewed`，`DraftEditor::send`（作曲窗口改用它）。
- `catmail-mcp`：`tools/sending.rs`（`send_draft`、`Asked`、问题的文字）；`tokens.rs` 的两个层级；`tools/mod.rs`
  的 `tier_of`、`Mail` 带上 `Asked`、与 `list_tools` 共用的协议版本判断、日志里记"asking the user"；instructions 改了发信的说法。
- CLI 的 `--tier` 说明、`catmail-frontend` 的层级文案；`docs/architecture/mcp.md`、`sending.md`、README。
- 验证：`catmail-core` 的 `a_previewed_send_goes_out_only_as_it_was_shown`、`the_editor_sends_the_draft_it_has_open`；`catmail-mcp` 的 `tests/sending.rs`
  （层级、确认后发、拒绝与取消、两步之间改了草稿、编的与别的令牌的句柄、客户端问不了、GUI 开着草稿）与
  `Asked` 的单元测试；工具快照加了 `send_draft`（夹具的令牌改为最高层级）。
