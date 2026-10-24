# 决定：Gmail 按标签建模，IMAP 改用 imap-proto 解析响应

- 日期：2026-10-24
- 类型：重构

## 背景

#121 修复了 Gmail 标签副本在搜索里重复、过时副本打不开的问题，做法是按 Message-ID 在搜索里去重、回放
移动后去问其他标签里的副本。#120 记下了用 `X-GM-MSGID` 精确认同一封的代价：imap-codec 的 FETCH 数据项
是封闭的 enum，发不出也收不进 `X-GM-*`。

需求随后明确：要能区分标签和真正的文件夹，同一封邮件只有一条记录；不考虑重写的代价，以可维护性和可扩展性
为准。这一步只记下决定，不改代码。

## 决策

- **一行是服务器上的一封邮件**：一个位置（文件夹, UIDVALIDITY, UID）加零或多个标签。普通 IMAP 账户上
  所有可选中的邮箱都是位置，副本不合并；Gmail（`X-GM-EXT-1`）上位置只有"所有邮件"、已删除、垃圾邮件，
  其余是标签，按 `X-GM-MSGID` 一封一行。两者是同一套模型，不另做一套 Gmail 专用的存储和同步。
- 放弃的方案：继续按文件夹存、在搜索和回放里按 Message-ID 补（#121 的做法）。它满足不了"一条记录"，
  每多一处要感知副本的地方就多一个例外；Message-ID 也可以没有或重复。
- **普通账户不合并副本**：复制到两个文件夹的是两封邮件，flags 各自独立、能分别删除，服务器上就是这样。
- **IMAP 库改用 imap-proto 只解析响应**，命令编码和连接驱动自己写。放弃的方案：
  - 本地补丁 imap-codec 加 `ext_gmail` 再提上游：封闭的 enum 意味着以后每个扩展都要再补一次，上游
    一直是 alpha；imap-next 的状态机已经绕过了四处。
  - `Connection` 上的裸命令通道：Gmail 主要的同步 FETCH 都要走它，等于另写一个解析器。
  - async-imap：解析器同样是 imap-proto，但它持有连接，又要在别人的状态机外面打补丁。
  - 自己写响应解析器：语法大、安全上敏感，不值得。
- 几项 Gmail 的行为（只改标签时 MODSEQ 变不变、IDLE 能看到什么、移到已删除后标签剩什么等）先用真实账户
  验证，再写代码。

## 方案

- 新增 `docs/architecture/labels.md`：模型、存储（`folders.kind`、`message_labels`、`gm_msgid` /
  `gm_thrid`）、同步（三个位置作为一个单元，按 `X-GM-MSGID` 搬行）、操作队列（标签操作、动作到操作的
  映射）、草稿与已发送、待验证的问题和迁移步骤。
- `docs/architecture/protocol.md` 的"IMAP 库"写明改用 imap-proto 的理由与做法；`architecture.md` 改写
  "邮件按文件夹存储"一节；`storage.md`、`op-queue.md` 里 #121 的做法标为过渡；`sending.md` 的已知问题
  指向新文档。
- AGENTS.md：规则改为 "A message row is one server message"，注明尚未实现、`LABEL_ACCOUNTS` 那套不要再
  扩展；crate 表注明协议层将改用 imap-proto。
- 只改文档，没有代码改动；用脚本检查了文档里所有相对链接和锚点都能解析。
- 用测试 Gmail 账户验证了设计依赖的行为（一次性的验证程序，基于 imap-proto，不进仓库），结果记在
  `labels.md` 的"验证结果"。据此调整了设计：
  - 只改标签也会让 MODSEQ 变大，标签随 flags 一起用 `CHANGEDSINCE` 拉；
  - 在"所有邮件"上 IDLE 收不到标签变化，后台同步仍在 INBOX 上 IDLE；
  - 移出已删除要 MOVE 到"所有邮件"，COPYUID 才是位置的 UID；
  - "已加星标"是 `\Flagged` 的视图，不存标签行；
  - 隐藏的标签也建一行（`listed = 0`）；
  - "所有邮件"不在 IMAP 中显示时按普通账户同步，所以 Gmail 账户要求 LIST 里有 `\All`，AGENTS.md 也跟着改了。
  - imap-proto 不还原带引号字符串里的转义，写进了 `protocol.md`，第 3 步要处理。
