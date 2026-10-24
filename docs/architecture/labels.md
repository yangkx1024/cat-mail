# 位置与标签（Gmail）

[← 架构](../architecture.md)

**状态：已决定，尚未实现。** 现在的代码里 Gmail 的每个标签仍是一个文件夹，同一封邮件在每个标签下各存
一行（见[存储 / 全文搜索](storage.md#全文搜索)、[操作队列 / Gmail 的标签](op-queue.md#gmail-的标签)）。
本文是迁移的目标，按文末的[步骤](#迁移步骤)实现，做完一步就把对应文档改成现状。下面关于 Gmail 行为的
说法都已在真实账户上验证过（见[验证结果](#验证结果)）。

## 目标

- **区分标签和文件夹。** 侧栏分开显示，邮件显示它带的标签。
- **一封邮件一行。** Gmail 上同一封邮件在 INBOX、"所有邮件"、"重要"里只有一行，搜索、计数、flags
  都不必再处理副本。

## 模型

`messages` 的一行是**服务器上的一封邮件**：它有且只有一个**位置**（文件夹, UIDVALIDITY, UID），另有零或
多个**标签**。`folders.kind` 区分两类邮箱：`location`（位置）和 `label`（标签）。

| | 普通 IMAP 账户 | Gmail 账户 |
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
  另外两个位置。`X-GM-MSGID` 在三个位置之间移动时不变。
- **UIDVALIDITY 的规则只作用于位置，不变。** 标签邮箱本地不同步、不记 UIDVALIDITY 和游标；持久化的东西
  只要指向远端 UID，仍然同时记下邮箱和它的 UIDVALIDITY（草稿的远端副本见[下文](#草稿与已发送)）。
- **Gmail 账户** = 有能力 `X-GM-EXT-1`，**并且** LIST 里有 `\All`。不看有没有 `\All` 来认 Gmail：
  Dovecot 等也能配置虚拟的 `\All` / `\Flagged` 邮箱，那里它们是普通位置，里面的行与原文件夹的行重复，
  所以普通账户的搜索不包括 `all`、`flagged` 角色的文件夹（它们只汇总其他文件夹，每封邮件在别处都有一行）。
- **"所有邮件"不在 IMAP 中显示时**（Gmail 设置里可以关掉），它不在 LIST 里，EXAMINE 也被拒绝
  （`NO [NONEXISTENT]`），只能按普通账户同步，并提示用户在 Gmail 里打开它。模式记在账户上；用户改了
  设置、模式变化时，这个账户的本地邮件像 UIDVALIDITY 变化那样整个重建，待回放的操作标为 `conflict`。

## 存储

改 `0001_init.sql`（项目未发布），已有的本地数据库要重建。

- `folders.kind`：`location` / `label`，对账文件夹时按上面的规则写入。`folders.listed`：LIST 里有没有它。
- `messages.gm_msgid`、`gm_thrid`：可空，Gmail 账户上 `UNIQUE (account_id, gm_msgid)`。会话里第一封的
  `X-GM-THRID` 等于它自己的 `X-GM-MSGID`。
- `message_labels (message_id, folder_id, account_id, list_date)`：主键 (message_id, folder_id)，到
  `messages`、`folders` 都用 `(id, account_id)` 复合外键。`list_date` 冗余一份邮件的排序键（日期不变），
  索引 `(folder_id, list_date, message_id)`：标签的翻页与文件夹一样是两次定位（见
  [存储 / 列表顺序与分页](storage.md#列表顺序与分页)），不必 JOIN 之后再排序。
- **计数**：标签的 `unread_count` / `total_count` 由 `message_labels` 的插入、删除触发器，以及
  `messages` 上 `\Seen` 变化时对它的每个标签更新，每次变化 O(标签数)。
- **`X-GM-LABELS` 到标签行。** 用户标签是 atom，就是 LIST 里的邮箱名（修改版 UTF-7），按 `remote_name`
  对应。系统标签是带引号的字符串（`"\\Inbox"`，解出来是 `\Inbox`），按角色对应：`\Inbox` → INBOX、
  `\Sent`、`\Draft` → `\Drafts`、`\Important`。在 Gmail 设置里被隐藏的标签不在 LIST 里，但仍出现在
  `X-GM-LABELS` 里：也建一行标签，`listed = 0`，界面不显示。用户以后让它显示时只改这个标记，不必重新
  拉一遍"所有邮件"的标签。
- **"已加星标"是 `\Flagged` 的视图**，不存标签行：Gmail 在邮件有 `\Flagged` 时才在 `X-GM-LABELS` 里列出
  `\Starred`，两者总是一致，再存一份就要在本地改 flags 时同步维护两处。

## 同步

- **对账文件夹**照常 LIST，按能力写 `kind`。标签邮箱不 EXAMINE、不同步；打开标签就是本地查询
  `message_labels`。
- **三个位置作为一个单元同步**：另一个客户端把邮件移到已删除时，它在"所有邮件"里消失、在已删除里以新
  UID 出现，`X-GM-MSGID` 不变。所以先处理三个位置里新出现的邮件：`X-GM-MSGID` 已有的就把那一行搬到新
  位置（正文、附件、全文索引随行保留），没有才插入；三个位置都对完之后，才删除在原位置消失、也没有被搬走
  的行。已删除和垃圾邮件通常很小，有 CONDSTORE 时没变化就只是一次 EXAMINE。
- **标签的变化**随 flags 一起拉：只改标签也会让邮件的 MODSEQ 变大，所以有 CONDSTORE 时一条
  `UID FETCH 1:* (FLAGS X-GM-LABELS) (CHANGEDSINCE <modseq>)` 拿到两者的变化。
- **删除**：Gmail 没有 QRESYNC，拿不到 VANISHED，删除仍靠现有的做法（条数对不上时 `UID SEARCH` 求差集，
  见[同步](sync.md#流程)）。
- **只在三个位置里 FETCH `X-GM-LABELS`**：在标签邮箱里 FETCH 时，Gmail 的答复不含这个邮箱自己的标签
  （在 INBOX 里看不到 `\Inbox`，在"重要"里看不到 `\Important`）。
- **后台同步仍在 INBOX 上 IDLE**：在"所有邮件"上 IDLE 收不到任何标签变化（包括进出 INBOX），在 INBOX 上
  能收到新邮件、flags、进出 INBOX（EXISTS / EXPUNGE）。被唤醒后同步的是三个位置，不是 INBOX 标签，
  `CHANGEDSINCE` 顺带拿到其他标签的变化；只改了其他标签、与 INBOX 无关的变化等下一次完整同步。Gmail 大约
  每 30 秒才推送一次，变化要过二三十秒才到。
- **打开一个标签**时的同步同样是三个位置的增量同步。

## 操作队列

操作仍然绑定位置的（文件夹, UIDVALIDITY, UID），在 SELECT 下核对 UIDVALIDITY 后回放。

- **新增标签操作**：本地立即改 `message_labels`，回放 `UID STORE <uid> +X-GM-LABELS (…)` /
  `-X-GM-LABELS (…)`。与 flags 一样只记实际改变的标签，被拒绝时按同样的方式逆推撤销。只加减，从不整体
  替换，被隐藏的标签不受影响。
- **用户的动作变成哪些操作，由引擎决定**（规则在引擎，见[架构](../architecture.md#规则在引擎措辞在前端)）：

  | 动作 | 普通账户 | Gmail |
  |---|---|---|
  | 归档（在 INBOX 里） | MOVE 到归档 | 去掉 `\Inbox` |
  | 移到某个邮箱 | MOVE | 在标签里：加上目标标签、去掉当前标签 |
  | 删除 | MOVE 到已删除 | MOVE 到已删除（位置变化） |
  | 在已删除里永久删除 | `\Deleted` + EXPUNGE | 同左，在已删除这个位置里 |
  | 移出已删除 / 垃圾邮件 | MOVE | MOVE 到"所有邮件"，再加上目标标签 |
  | 加 / 去标签 | 无 | 标签操作 |

- **移到已删除**时 Gmail 去掉 `\Inbox`，保留 `\Important` 和用户标签；**移到垃圾邮件**时去掉 `\Inbox` 和
  `\Important`，只留用户标签。本地照此改 `message_labels`，同步会再核对一遍。
- **移出已删除 / 垃圾邮件要 MOVE 到"所有邮件"**：MOVE 到 INBOX 也能恢复，但 COPYUID 给的是 INBOX 这个
  标签邮箱里的 UID，不是位置的 UID。MOVE 到"所有邮件"的 COPYUID 就是新位置，同一行改为新的
  （文件夹, UID），与普通账户相同。COPYUID 按位置配对（RFC 4315）在 Gmail 上成立，目标 UID 可能倒序列出。
- **从不在标签邮箱里 `STORE \Deleted`**：Gmail 默认开着自动删除，`+\Deleted` 立即 EXPUNGE，效果是去掉
  这个标签，邮件仍在"所有邮件"里。去标签一律用 `-X-GM-LABELS`。
- 删掉现有的按 Message-ID 找副本的机制：`PendingOps::label_copies`、`Engine::add_label_copies`、
  `sync::drop_vanished`、搜索里的 `SEARCH_PICK`、`LABEL_ACCOUNTS`。邮件只有一行，没有副本要找。

## 草稿与已发送

- **草稿**：APPEND 到草稿邮箱（Gmail 上是标签），APPENDUID 给的是那个标签邮箱里的 UID 和 UIDVALIDITY；
  这封邮件在"所有邮件"里带 `\Draft` 标签。远端副本仍记（草稿邮箱, UIDVALIDITY, UID）：标签邮箱在服务器上
  照样有 UID，只是本地不同步它。在草稿邮箱里 `\Deleted` + EXPUNGE 会彻底删掉这封草稿（"所有邮件"和
  已删除里都没有了），所以现有的"按 UID 删除旧副本"照常可用。
- **按 Message-ID 找**（APPEND 答复丢失后、保存到已发送之前）：Gmail 的 `SEARCH HEADER` 按词匹配，不是
  RFC 3501 要求的子串匹配，但整个 Message-ID（带尖括号）能搜到，现有的做法可用。
- **已发送**：Gmail 自动把经 SMTP 发出的邮件存进已发送，再 APPEND 会多一份（[发信](sending.md#保存到-sent)
  里的已知问题）。Gmail 账户跳过这一步。

## 界面

侧栏把位置和标签分开列；系统标签（INBOX、已发送、草稿等）仍按角色排在前面，`listed = 0` 的标签不列。
邮件列表和读信区显示邮件的用户标签。"所有邮件"没有在 IMAP 中显示的 Gmail 账户提示用户打开它。具体样式
在 GUI 那一步定。

## IMAP 库

`X-GM-*` 要靠协议层支持，这是改用 imap-proto 的原因之一，见 [IMAP / SMTP 连接](protocol.md#imap-库)。

## 验证结果

2026-10-24 用一个测试 Gmail 账户（默认设置：开启自动删除，删除时"存档"）验证，验证程序用 imap-proto
解析了全部答复，没有一行失败。

| 问题 | 结果 |
|---|---|
| 能力 | `X-GM-EXT-1`、`CONDSTORE`、`MOVE`、`UIDPLUS`、`UTF8=ACCEPT`、`ESEARCH`、`LIST-STATUS`、`LITERAL-`；没有 QRESYNC |
| LIST | 三个位置带 `\All` / `\Trash` / `\Junk`，另有 `\Sent`、`\Drafts`、`\Flagged`、`\Important`（RFC 8457）；`[Gmail]` 是 `\Noselect` |
| 只改标签时 MODSEQ | 变大，`CHANGEDSINCE` 拿得到 |
| IDLE | "所有邮件"：新邮件、flags、移到已删除，标签变化一概没有；INBOX：新邮件、flags、进出 INBOX。约每 30 秒推送一次 |
| 移到已删除 / 垃圾邮件 | `X-GM-MSGID` 不变；已删除去掉 `\Inbox`，垃圾邮件去掉 `\Inbox` 和 `\Important`，用户标签保留；移回后在"所有邮件"里得到新 UID，标签恢复 |
| `X-GM-LABELS` 的写法 | 系统标签是带引号的字符串（`"\\Inbox"`、`"\\Important"`、`"\\Starred"`、`"\\Draft"`），用户标签是 atom（修改版 UTF-7 的邮箱名）；`\Starred` 与 `\Flagged` 一致；在标签邮箱里 FETCH 不含这个邮箱自己的标签 |
| 草稿邮箱里 EXPUNGE | 草稿彻底删除 |
| 用户标签邮箱里 `\Deleted` | 自动删除下立即 EXPUNGE，只去掉这个标签 |
| 隐藏"所有邮件" / 标签 | 不在 LIST 里，EXAMINE 回 `NO [NONEXISTENT]`；隐藏的标签仍出现在 `X-GM-LABELS` 里 |
| 多封邮件的 MOVE | COPYUID 按位置配对正确，目标 UID 可能倒序列出（`359:362 337,336,335,334`） |
| `SEARCH HEADER` | 按词匹配，不是子串；完整的 Message-ID 能搜到 |

没有验证的：经 SMTP 发出的邮件自动进已发送（Gmail 的公开行为，未在本账户上发信）；非默认的删除设置
（"移到已删除"、"立即永久删除"）下标签邮箱里 `\Deleted` 的效果——我们不在标签邮箱里 STORE `\Deleted`，
不依赖它。

## 迁移步骤

1. ~~记下决定（本文、[IMAP 库](protocol.md#imap-库)、AGENTS.md）。~~
2. ~~用真实 Gmail 验证上面几点，同时试用 imap-proto。~~
3. ~~协议层改用 imap-proto，`Session` 的接口不变，现有的假服务器和 Dovecot 测试做回归。~~ imap-proto 暂时来自
   main，0.18.0 发版后换成 crates.io 的版本（见 [IMAP 库](protocol.md#imap-库)）。
4. 存储与同步：`folders.kind` / `listed`、`message_labels`、`gm_msgid` / `gm_thrid`、账户的模式；假服务器
   支持 `X-GM-EXT-1`。CI 里没有真的 Gmail，与真实服务器的行为只能手动验证。
5. 操作队列：标签操作、动作到操作的映射；删除按 Message-ID 找副本的机制。
6. GUI：侧栏分开位置和标签，显示邮件的标签。
