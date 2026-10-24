# 协议层改用 imap-proto 解析响应

- 日期：2026-10-24
- 类型：重构

## 背景

按[位置与标签](../../docs/architecture/labels.md)的迁移步骤 3：Gmail 要用 `X-GM-*`，imap-codec 的封闭 enum
发不出也收不进；imap-next 的状态机已经绕过了几处。决定改为 imap-proto 只解析响应，命令和连接自己写。

## 决策

- imap-proto 0.17.0 不还原带引号字符串里的转义，解析出来的值又看不出是带引号的字符串还是 literal，在我们
  这边补不可靠。上游 main 已修（#209），但 0.18.0 未发版。用户选择：开发时用 `[patch.crates-io]` 固定到
  main 的 commit，不写临时补丁，0.18.0 发版后换成 crates.io 的版本；暂不去上游开 issue 催发版。
  在那之前 `cargo deny`（sources）失败，项目未发布，用户决定先合并。
- 放弃的方案：0.17.0 加我们这边的还原（对 literal 会改坏内容）；把 imap-proto vendor 进仓库。
- APPEND 在发出邮件前被 NO：现在命令就此结束、连接照常可用，是普通的 `Error::Command`。删掉
  `Error::Aborted` 和草稿上传里遇到它时换新连接的分支。
- QQ 邮箱 INTERNALDATE 的修补不再改字节：imap-proto 不校验它，日期交给 jiff 解析（`%d` 接受一位数的日）。
  `quirks.rs` 删除，literal 的跟踪移到 `framing.rs`；只有 LF 的行尾在分帧时补 CR（imap-proto 只认 CRLF）。
- 评审（codex）指出两处：
  - imap-proto 把格式坏掉的已知 FETCH 项读成 `Unknown`，整行解析成功，`Incomplete` 就漏掉了：`FLAGS NIL`
    让 `fetch_flags` 少一封却成功返回，CONDSTORE 下 HIGHESTMODSEQ 前进后这个变化永远丢失；坏的 BODYSTRUCTURE
    让 `fetch_structure` 返回 `None`，大邮件被当成服务器上已没有。改为请求过的项必须都在，缺了就是
    `Incomplete`；拉邮件头时缺 BODYSTRUCTURE 的照旧不带它重取。没有选"FETCH 里有 `Unknown` 就算不完整"：
    服务器主动多给的新扩展项应当放过。
  - imap-proto 的 CAPABILITY 要求有 IMAP4rev1，只声明 IMAP4rev2 的服务器连不上。连接层在解析失败时自己读
    `* CAPABILITY` 和 `[CAPABILITY …]`。根本的修复应在上游。
- 命令名改为线上的完整写法（`UID SEARCH`、`UID FETCH`），出现在错误信息里。
- 搜索 Message-ID 时用带引号的字符串，不用 atom。
- 代价：imap-proto 把 BODYSTRUCTURE 里的字符串按 UTF-8 有损转换，原始 8 位字节的附件文件名在按部分下载的
  大邮件里会变成 `U+FFFD`。写进了 `protocol.md`。

## 方案

- `catmail-proto/src/imap/`：新增 `command.rs`（参数选 atom / 带引号 / literal，按 LITERAL+ / LITERAL- /
  同步编码，命令字节 `Zeroizing`）、`framing.rs`（切出完整响应、100 MiB 上限、补 CR）；重写
  `connection.rs`（tag、续行、IDLE、AUTHENTICATE、untagged 状态与响应码，不认识的响应码按名字记下，如
  `NOMODSEQ`）、`session.rs`（接口不变）、`body.rs`。
- 工作区去掉 `imap-next` 和为 imap-codec 补的 `nom`。
- `catmail-core/src/draft_sync.rs`：去掉 `Aborted` 分支和只为重连读的账户。
- 文档：`protocol.md`（IMAP 库、连接层）、`sending.md`、`sync.md`、`bodies.md`、`architecture.md`、
  `labels.md`、AGENTS.md 的 crate 表。
- 验证：`cargo test --workspace` 全部通过；Dovecot 与 submission 真实服务器测试通过（Dovecot 有
  LITERAL+）；用 CLI 对 Gmail 测试账户添加账户、同步、读信、搜索，之后删除。新增单元测试：命令编码、分帧、
  未知响应码、INTERNALDATE、LIST 属性、flags 大小写、section 校验；提前拒绝的 APPEND 测试改为期望普通拒绝、
  连接可用；两个测试按新的命令名 / 带引号的 Message-ID 更新。评审两处各有假服务器测试（新增故障 `broken_flags`、
  `imap4rev2_only`；`broken_bodystructure` 现在走的是缺项这条路），去掉修复后都失败。
