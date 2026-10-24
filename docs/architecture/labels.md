# 位置与标签（Gmail）

[← 架构](../architecture.md)

**状态：已决定，尚未实现。** 现在的代码里 Gmail 的每个标签仍是一个文件夹，同一封邮件在每个标签下各存
一行（见[存储 / 全文搜索](storage.md#全文搜索)、[操作队列 / Gmail 的标签](op-queue.md#gmail-的标签)）。
本文是迁移的目标，按文末的[步骤](#迁移步骤)实现，做完一步就把对应文档改成现状。

## 目标

- **区分标签和文件夹。** 侧栏分开显示，邮件显示它带的标签。
- **一封邮件一行。** Gmail 上同一封邮件在 INBOX、"所有邮件"、"重要"里只有一行，搜索、计数、flags
  都不必再处理副本。

## 模型

`messages` 的一行是**服务器上的一封邮件**：它有且只有一个**位置**（文件夹, UIDVALIDITY, UID），另有零或
多个**标签**。`folders.kind` 区分两类邮箱：`location`（位置）和 `label`（标签）。

| | 普通 IMAP 账户 | Gmail 账户（`X-GM-EXT-1`） |
|---|---|---|
| 位置 | 所有可选中的邮箱 | 只有 `\All`、`\Trash`、`\Junk` 三个 |
| 标签 | 无 | 其余邮箱：INBOX、已发送、草稿、重要、用户标签 |
| 一行的身份 | （文件夹, UID） | `X-GM-MSGID` |
| 同步哪些邮箱 | 所有位置 | 三个位置，FETCH 另带 `X-GM-MSGID X-GM-THRID X-GM-LABELS` |
| flags | 每行自己的 | 每行自己的（Gmail 的 flags 本来就属于邮件，不属于标签） |

- **只有一套模型。** 普通账户就是"没有标签"的特例，存储、同步、操作队列走同一套代码，差别只在两处：
  哪些邮箱是位置，FETCH 多带哪些属性。
- **普通账户上不合并副本。** 复制到两个文件夹的是两封邮件：flags 各自独立、可以分别删除，在服务器上
  就是这样。Message-ID 不可靠（可以没有，也可以重复），不能用来认定"同一封"。
- **Gmail 上也必须有位置。** UID 只在一个邮箱里有意义，操作要在某个邮箱里按 UID 执行。"所有邮件"包含
  除已删除、垃圾邮件之外的每一封，邮件留在里面时 UID 不变；已删除和垃圾邮件不在"所有邮件"里，所以是
  另外两个位置。
- **UIDVALIDITY 的规则只作用于位置，不变。** 标签邮箱本地不同步、不记 UIDVALIDITY 和游标；持久化的东西
  只要指向远端 UID，仍然同时记下邮箱和它的 UIDVALIDITY（草稿的远端副本见[下文](#草稿与已发送)）。
- **认 Gmail 看能力 `X-GM-EXT-1`**，不看有没有 `\All`。Dovecot 等也能配置虚拟的 `\All` / `\Flagged`
  邮箱，那里它们是普通位置，里面的行与原文件夹的行重复：普通账户的搜索不包括 `all`、`flagged` 角色的
  文件夹（它们只汇总其他文件夹，每封邮件在别处都有一行）。

## 存储

改 `0001_init.sql`（项目未发布），已有的本地数据库要重建。

- `folders.kind`：`location` / `label`，对账文件夹时按上面的规则写入。
- `messages.gm_msgid`、`gm_thrid`：可空，Gmail 账户上 `UNIQUE (account_id, gm_msgid)`。
- `message_labels (message_id, folder_id, account_id, list_date)`：主键 (message_id, folder_id)，到
  `messages`、`folders` 都用 `(id, account_id)` 复合外键。`list_date` 冗余一份邮件的排序键（日期不变），
  索引 `(folder_id, list_date, message_id)`：标签的翻页与文件夹一样是两次定位（见
  [存储 / 列表顺序与分页](storage.md#列表顺序与分页)），不必 JOIN 之后再排序。
- **计数**：标签的 `unread_count` / `total_count` 由 `message_labels` 的插入、删除触发器，以及
  `messages` 上 `\Seen` 变化时对它的每个标签更新，每次变化 O(标签数)。
- 系统标签对应到文件夹：`X-GM-LABELS` 里的 `\Inbox`、`\Sent`、`\Draft`、`\Important` 等对应有相应角色的
  标签邮箱，其余按名称对应。"已加星标"是 `\Flagged` 的视图，不是标签（待验证，见下）。

## 同步

- **对账文件夹**照常 LIST，按能力写 `kind`。标签邮箱不 EXAMINE、不同步；打开标签就是本地查询
  `message_labels`。
- **三个位置作为一个单元同步**：另一个客户端把邮件移到已删除时，它在"所有邮件"里消失、在已删除里以新
  UID 出现，`X-GM-MSGID` 不变。所以先处理三个位置里新出现的邮件：`X-GM-MSGID` 已有的就把那一行搬到新
  位置（正文、附件、全文索引随行保留），没有才插入；三个位置都对完之后，才删除在原位置消失、也没有被搬走
  的行。已删除和垃圾邮件通常很小，有 CONDSTORE 时没变化就只是一次 EXAMINE。
- **标签的变化**：有 CONDSTORE 时 `UID FETCH 1:* (FLAGS X-GM-LABELS) (CHANGEDSINCE <modseq>)`，与 flags
  一起拉。前提是只改标签也会让 MODSEQ 变大（待验证）；不会的话，其他客户端改的标签只能按窗口全量拉
  `X-GM-LABELS` 才能发现，"所有邮件"有十几万封时代价很高，要另想办法。
- **后台同步**：IDLE 盯哪个邮箱（INBOX 标签还是"所有邮件"）取决于 Gmail 在哪个邮箱上报告新邮件和标签
  变化（待验证）。被唤醒后同步的是三个位置，不是 INBOX 标签。
- **打开一个标签**时的同步同样是三个位置的增量同步。

## 操作队列

操作仍然绑定位置的（文件夹, UIDVALIDITY, UID），在 SELECT 下核对 UIDVALIDITY 后回放。

- **新增标签操作**：本地立即改 `message_labels`，回放 `UID STORE <uid> +X-GM-LABELS (…)` /
  `-X-GM-LABELS (…)`。与 flags 一样只记实际改变的标签，被拒绝时按同样的方式逆推撤销。
- **用户的动作变成哪些操作，由引擎决定**（规则在引擎，见[架构](../architecture.md#规则在引擎措辞在前端)）：

  | 动作 | 普通账户 | Gmail |
  |---|---|---|
  | 归档（在 INBOX 里） | MOVE 到归档 | 去掉 `\Inbox` |
  | 移到某个邮箱 | MOVE | 在标签里：加上目标标签、去掉当前标签 |
  | 删除 | MOVE 到已删除 | MOVE 到已删除（位置变化） |
  | 在已删除里永久删除 | `\Deleted` + EXPUNGE | 同左，在已删除这个位置里 |
  | 移出已删除 / 垃圾邮件 | MOVE | MOVE 回"所有邮件"，再加上目标标签（待验证） |
  | 加 / 去标签 | 无 | 标签操作 |

- 删掉现有的按 Message-ID 找副本的机制：`PendingOps::label_copies`、`Engine::add_label_copies`、
  `sync::drop_vanished`、搜索里的 `SEARCH_PICK`、`LABEL_ACCOUNTS`。邮件只有一行，没有副本要找。

## 草稿与已发送

- **草稿**：APPEND 到草稿邮箱（Gmail 上是标签），APPENDUID 给的是那个标签邮箱里的 UID。远端副本仍记
  （草稿邮箱, UIDVALIDITY, UID）：标签邮箱在服务器上照样有 UID 和 UIDVALIDITY，只是本地不同步它。在标签
  邮箱里 EXPUNGE 是去掉标签还是删除邮件（Gmail 对草稿可能不同），决定了旧副本怎么删（待验证）。
- **已发送**：Gmail 自动把经 SMTP 发出的邮件存进已发送，再 APPEND 会多一份（[发信](sending.md#保存到-sent)
  里的已知问题）。Gmail 账户跳过这一步。

## 界面

侧栏把位置和标签分开列；系统标签（INBOX、已发送、草稿等）仍按角色排在前面。邮件列表和读信区显示邮件的
用户标签。具体样式在 GUI 那一步定。

## IMAP 库

`X-GM-*` 要靠协议层支持，这是改用 imap-proto 的原因之一，见 [IMAP / SMTP 连接](protocol.md#imap-库)。

## 待验证

写代码之前用真实的 Gmail 账户确认，结果记回本文：

1. 只改标签时 MODSEQ 是否变大，`CHANGEDSINCE` 能否拿到标签的变化。
2. 在"所有邮件"和 INBOX 上 IDLE，分别能收到哪些变化（新邮件、标签、flags、移到已删除）。
3. 移到已删除、垃圾邮件后 `X-GM-MSGID` 是否不变，`X-GM-LABELS` 还剩什么；从已删除移回之后呢。
4. `X-GM-LABELS` 里系统标签的写法（`\Inbox`、`\Starred` 等），用户标签是否用修改版 UTF-7。
5. 草稿邮箱里 `\Deleted` + EXPUNGE 的效果。
6. 用户在 Gmail 设置里让"所有邮件"或某些标签不在 IMAP 中显示时 LIST 和 `X-GM-LABELS` 是什么样；"所有
   邮件"不可见时只能把这个账户当普通账户。

## 迁移步骤

1. 记下决定（本文、[IMAP 库](protocol.md#imap-库)、AGENTS.md）。
2. 用真实 Gmail 验证上面几点，同时试用 imap-proto。
3. 协议层改用 imap-proto，`Session` 的接口不变，现有的假服务器和 Dovecot 测试做回归。
4. 存储与同步：`folders.kind`、`message_labels`、`gm_msgid` / `gm_thrid`；假服务器支持 `X-GM-EXT-1`。
   CI 里没有真的 Gmail，与真实服务器的行为只能手动验证。
5. 操作队列：标签操作、动作到操作的映射；删除按 Message-ID 找副本的机制。
6. GUI：侧栏分开位置和标签，显示邮件的标签。
