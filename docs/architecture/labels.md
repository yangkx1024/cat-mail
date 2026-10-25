# 位置与标签（Gmail）

[← 架构](../architecture.md)

**状态：部分实现。** 存储、协议、同步和操作队列已经按这个模型（[步骤](#迁移步骤) 4、5）：Gmail 账户只
同步三个位置，一封邮件一行，移动和归档改的是标签。界面还没有分开显示位置和标签（步骤 6）。本文是迁移的
目标，按文末的步骤实现，做完一步就把对应文档改成现状。下面关于 Gmail 行为的说法都已在真实账户上验证过
（见[验证结果](#验证结果)）。

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
  `\Starred`，两者总是一致，再存一份就要在本地改 flags 时同步维护两处。列出它走部分索引
  `messages_flagged (account_id, list_date) WHERE flags & 2`，与文件夹一样是两次定位。
- **标签只显示"所有邮件"里的邮件。** 移到已删除、垃圾邮件的邮件保留标签行（移回时 Gmail 恢复这些标签），
  但标签和"已加星标"的列表、计数都不算它们，与 Gmail 网页版一致。视图 `all_mail` 给出"所有邮件"这个
  位置，列表、计数触发器都按它过滤。**这一点没有在真实账户上验证**（见[验证结果](#验证结果)）。
- **计数触发器**（`labels_count_*`）：标签行的插入、删除，邮件的插入、删除，邮件 `\Seen` / `\Flagged`
  变化或进出"所有邮件"时更新它所在的标签和"已加星标"。邮件行删除之前（`BEFORE DELETE`）先删它的标签行，
  这时触发器还看得到邮件；它的位置文件夹也必须还在，所以文件夹删除之前由触发器先删它的邮件，不走级联。
  视图 `all_mail` 和 `starred` 各有部分索引：每封邮件的变化都要查它们。
  文件夹计的是什么取决于它的 `kind` 和角色，触发器不管这两列：对账时有文件夹的 `kind` 或角色变了（如账户
  模式变了），或新出现了"已加星标"（在 IMAP 里隐藏过，它要显示已存的带 `\Flagged` 的邮件），整个账户的
  计数重算一遍（`Folders::recount`）。新出现的"所有邮件"是空的，不必重算。
- **对账文件夹**：LIST 里没有了的标签，还有邮件带着就留下、`listed = 0`，没有就删除。位置变成标签时
  像 `\Noselect` 那样结束它的 UIDVALIDITY 代（邮件删除，待回放的操作成为 `conflict`）；标签变成位置时
  删掉它的标签行。
- **"重要"的角色**来自 LIST 属性 `\Important`（RFC 8457，`FolderRole::Important`）。`X-GM-LABELS` 里
  不认识的系统标签（不在 `\Inbox`、`\Sent`、`\Draft`、`\Important`、`\Starred` 之中）忽略；系统标签
  找不到对应角色的文件夹（在 Gmail 设置里被隐藏）时也忽略，因为不知道它的邮箱名。它的邮箱后来又出现时，
  邮件的 MODSEQ 不会因此变大，所以对账时清空各个位置的 HIGHESTMODSEQ，下次同步重拉全部 flags 和标签。
- **`X-GM-MSGID` / `X-GM-THRID` 是无符号 64 位**，按位存成 SQLite 的 INTEGER（与 MODSEQ 相同），只比较
  相等，不比较大小。

## 同步

代码在 `crates/catmail-core/src/sync.rs`，流程的其余部分与普通账户相同（见[同步](sync.md)）。

- **对账文件夹**照常 LIST。有 `X-GM-EXT-1` 且 LIST 里有 `\All` 时账户是 Gmail 模式（`sync::mode_of`），
  `\All`、`\Trash`、`\Junk` 是位置，其余可选中的邮箱是标签（`\Noselect` 的 `[Gmail]` 仍是不同步的层级
  节点）。Gmail 模式下不按名称猜角色：Gmail 用 SPECIAL-USE 声明它有的每个角色，猜的话，已删除在 IMAP
  里隐藏时名为 Trash 的用户标签会顶替它。标签邮箱不 EXAMINE、不同步（`Folders::location_ids` 只列位置）；
  打开标签就是本地查询 `message_labels`。
- **模式变化**（`Accounts::set_mode` 报告变了）时在对账的同一个事务里结束每个位置的 UIDVALIDITY 代
  （`Folders::end_generations`）：一行代表的东西变了（每个位置一行，或每个 `X-GM-MSGID` 一行），已删除、
  垃圾邮件在两种模式下都是位置，也要重拉，好带上 `X-GM-MSGID`。待回放的操作成为 `conflict`。新账户第一次
  同步时也是"变化"，那时还什么都没有。
- **三个位置作为一个单元同步**：另一个客户端把邮件移到已删除时，它在"所有邮件"里消失、在已删除里以新
  UID 出现，`X-GM-MSGID` 不变。每个位置照常求差集、刷新 flags、拉新邮件（`Items::Gmail`），但消失的 UID
  先记在 `Unit` 里，三个位置都做完才删（普通账户上每个位置是一个单元，立即删）。拉到的新邮件 `X-GM-MSGID` 已有一行时，那一行改为新的
  （位置, UID）（`Messages::upsert`，报告里算作新增），正文、附件、全文索引随行保留；所以先出现在新位置
  的邮件，旧位置删除时已经不在那里。删除之前核对位置的 UIDVALIDITY 仍是记下时的那一代（完整同步按位置
  拿锁，其间别的同步可能开始了新的一代）。
  - 删除之前再看新到的（`sync::finish`）：移过去的邮件在新位置的 UID 一定不小于那里的 UIDNEXT，但可能
    在那个位置同步之后才移过去（例如从已删除移回"所有邮件"）。所以每个位置只看游标之后新到的（没有就
    只是一次 EXAMINE），拉下来，移过去的那一行随之搬走；一直看到某一遍哪里也没有新到的为止，最多
    `RECHECKS`（3）遍。这时还在原处的消失的邮件哪里都不在，才删除；看不到头就一个也不删，留给下次同步
    （条数对不上，会再找到）。已经搬走的不用再找：一个也不剩时不再看。
  - 有位置没同步完整（被拒绝，或拉邮件头时有邮件没拿到）时一个也不删：消失的邮件可能就在没拿到的那里。
    中途断线同样不删。下次同步条数对不上，会重新求差集找到它们；游标照常写（新邮件都存下了）。
  - 那一行正在本地移动（`uid` 为空，待回放的移动指着它在服务器上的位置）时不动它：在移动的来源里看到的
    就是它本身，在别处看到的说明服务器已经移动、本地的操作还没了结，这一批算不完整、不写游标，回放了结
    之后再同步。
  - 每个位置各自拿锁（完整同步，见[锁](sync.md#锁)），打开文件夹和后台同步的 INBOX 步骤整个单元持有锁。
    已删除和垃圾邮件通常很小，有 CONDSTORE 时没变化就只是一次 EXAMINE。
- **标签的变化**随 flags 一起拉：只改标签也会让邮件的 MODSEQ 变大，所以有 CONDSTORE 时一条
  `UID FETCH 1:* (FLAGS X-GM-LABELS) (CHANGEDSINCE <modseq>)` 拿到两者的变化。与 flags 一样，还没回放的
  标签改动叠加在服务器报告的标签之上（`Messages::set_labels`），否则归档之后、回放之前的同步会让 INBOX
  又出现。
- **删除**：Gmail 没有 QRESYNC，拿不到 VANISHED，删除仍靠现有的做法（条数对不上时 `UID SEARCH` 求差集，
  见[同步](sync.md#流程)）。
- **只在三个位置里 FETCH `X-GM-LABELS`**：在标签邮箱里 FETCH 时，Gmail 的答复不含这个邮箱自己的标签
  （在 INBOX 里看不到 `\Inbox`，在"重要"里看不到 `\Important`）。
- **后台同步仍在 INBOX 上 IDLE**：在"所有邮件"上 IDLE 收不到任何标签变化（包括进出 INBOX），在 INBOX 上
  能收到新邮件、flags、进出 INBOX（EXISTS / EXPUNGE）。被唤醒后同步的是三个位置（`sync::sync_inbox`），
  再 EXAMINE INBOX 接着 IDLE。IDLE 报告的是 INBOX 这个标签邮箱里的 UID，在位置里没有意义，所以 flags
  按 CONDSTORE 全部刷新（只拉变了的），`CHANGEDSINCE` 顺带拿到其他标签的变化；只改了其他标签、与 INBOX
  无关的变化等下一次完整同步。Gmail 大约每 30 秒才推送一次，变化要过二三十秒才到。
- **打开文件夹**（`MailClient::sync_folder`）在 Gmail 账户上不论打开的是哪个，同步的都是三个位置；三个位置
  都在 `max_age` 内同步过才算新鲜。打开的是标签时，报告是三个位置合起来的。
- **事件**：位置里的邮件变了，除了位置本身，也对显示它们的标签（变化前后的标签，带 `\Flagged` 的还有
  "已加星标"）发 `MessagesChanged`：`Messages` 记下它的写入（插入、搬行、flags 和标签、删除）改变了哪些
  标签的列表（`touched_labels`，与本地改动共用 `messages::shown`），前端照常只刷新正在看的那个文件夹。本地的标记、移动、删除和回放被拒绝
  时的撤销也一样（`PendingOps` 把显示这封邮件的标签记进 `Touched`）。
- **搜索**：邮件只有一行，不再按 Message-ID 合并。普通账户的搜索不包括 `all`、`flagged` 角色的文件夹
  （Dovecot 等虚拟的 `\All` / `\Flagged`，每封邮件在别处都有一行），用户在其中搜索时除外；Gmail 上
  "所有邮件"是位置，照常搜索。在标签或"已加星标"里搜索，范围与它的列表相同。

## 协议

- `Session::fetch_summaries` / `fetch_flags` 的 `Items::Gmail` 另带 `X-GM-MSGID X-GM-THRID X-GM-LABELS`
  （flags 只带 `X-GM-LABELS`）。请求过的项缺了与其他项一样算不完整：拉邮件头时这封邮件不返回，拉 flags
  时整条命令 `Incomplete`。标签放在 `FlagState::labels`，随 flags 一起走同一条存储路径
  （`Messages::update_flags`），`None` 表示没有拉，不动已存的标签。
- `Session::uid_store_labels`：`UID STORE <set> +X-GM-LABELS (…)` / `-X-GM-LABELS (…)`。系统标签按 flag
  的写法发（`\Inbox`），用户标签按 astring（必要时带引号或 literal）。"已加星标"不是标签，要求给它会返回
  `InvalidInput`，改用 `\Flagged`。
- `Session::uid_search_gm_msgid`：`UID SEARCH X-GM-MSGID <id>`。
- 没有 `X-GM-EXT-1`（`Session::is_gmail`）时，后两者返回 `Unsupported`；`Items::Gmail` 由调用方只在
  Gmail 账户的位置里使用。

## 操作队列

操作仍然绑定位置的（文件夹, UIDVALIDITY, UID），在 SELECT 下核对 op 自己记下的 UIDVALIDITY 后回放
（见[操作队列](op-queue.md)，代码在 `catmail-store/src/ops.rs` 与 `catmail-core/src/ops.rs`）。

- **标签操作**（`MailClient::label_messages`，op 的 `kind = 'label'`）：本地立即改 `message_labels`，
  回放 `UID STORE <uid> +X-GM-LABELS (…)` / `-X-GM-LABELS (…)`，在邮件的位置里。与 flags 一样只记实际
  改变的标签，被拒绝时按同样的方式逆推撤销（按标签）。只加减，从不整体替换，被隐藏的标签不受影响。
  "已加星标"不是标签，要求给它会被拒绝，改用 `\Flagged`。
- **用户的动作变成哪些操作，由引擎决定**（`MovePlan`，在 types 里，前端用同一个函数判断能否归档、
  可以移到哪里；见[架构](../architecture.md#规则在引擎措辞在前端)）。移动要知道用户在哪个标签里看着这封
  邮件（`move_messages` 的 `shown_in`，搜索结果为空）：

  | 动作 | 普通账户 | Gmail |
  |---|---|---|
  | 归档 | MOVE 到归档 | 在"所有邮件"里的去掉 `\Inbox`，已删除、垃圾邮件里的不动 |
  | 移到某个标签 | MOVE | 加上目标标签，在标签里看时去掉这个标签（"已加星标"除外：星标是 flag） |
  | 移到"所有邮件" | MOVE | 在标签里看时去掉这个标签（从 INBOX 移过去就是归档） |
  | 删除 | MOVE 到已删除 | MOVE 到已删除（位置变化），标签不动 |
  | 在已删除里永久删除 | `\Deleted` + EXPUNGE | 同左，在已删除这个位置里 |
  | 移出已删除 / 垃圾邮件 | MOVE | MOVE 到"所有邮件"，移到标签的再加上目标标签 |
  | 移到"已加星标" | 无 | 拒绝：改为加星标 |
  | 加 / 去标签 | 无 | 标签操作 |

- **移到已删除 / 垃圾邮件**时 Gmail 去掉 `\Inbox`，保留 `\Important` 和用户标签；**移到垃圾邮件**时去掉
  `\Inbox` 和 `\Important`，只留用户标签。`MovePlan` 把这些标签列为移动去掉的标签：本地立即去掉，并像
  别的标签改动一样入队，等移动做成后在新位置回放（服务器上已经去掉了，这条 `-X-GM-LABELS` 不改变什么，
  多一条命令）。这样服务器做的改动本地事先就知道：移动正在途中时用户又把邮件移回 INBOX，加回 `\Inbox`
  会照常入队；移动被拒绝时它们一起失败、恢复。
- **移出已删除 / 垃圾邮件要 MOVE 到"所有邮件"**：MOVE 到 INBOX 也能恢复，但 COPYUID 给的是 INBOX 这个
  标签邮箱里的 UID，不是位置的 UID。MOVE 到"所有邮件"的 COPYUID 就是新位置，同一行改为新的
  （文件夹, UID），与普通账户相同。COPYUID 按位置配对（RFC 4315）在 Gmail 上成立，目标 UID 可能倒序列出。
  目标标签在 MOVE 之后才加：在一次移动之后做的标签改动等这次移动，随邮件改指新位置，同一次回放里接着做
  （见[操作队列 / 回放](op-queue.md#回放)）；移动被拒绝时它一起失败。在已删除里给邮件加 `\Inbox` 会怎样
  没有验证过，不依赖它。
- **从不在标签邮箱里 `STORE \Deleted`**：Gmail 默认开着自动删除，`+\Deleted` 立即 EXPUNGE，效果是去掉
  这个标签，邮件仍在"所有邮件"里。所有操作都在邮件的位置里回放，去标签一律用 `-X-GM-LABELS`。唯一的例外
  是删除草稿的旧副本（见下文）。
- 按 Message-ID 找副本的过渡机制已经删除：邮件只有一行，没有副本要找。

## 草稿与已发送

- **草稿**：APPEND 到草稿邮箱（Gmail 上是标签），APPENDUID 给的是那个标签邮箱里的 UID 和 UIDVALIDITY；
  这封邮件在"所有邮件"里带 `\Draft` 标签。远端副本仍记（草稿邮箱, UIDVALIDITY, UID）：标签邮箱在服务器上
  照样有 UID，只是本地不同步它。在草稿邮箱里 `\Deleted` + EXPUNGE 会彻底删掉这封草稿（"所有邮件"和
  已删除里都没有了），所以现有的"按 UID 删除旧副本"照常可用。回放这条删除时核对的是操作自己记下的那一代
  （本地的标签没有 UIDVALIDITY），SELECT 之后别的代的操作成为 `conflict`。
- **按 Message-ID 找**（APPEND 答复丢失后、保存到已发送之前）：Gmail 的 `SEARCH HEADER` 按词匹配，不是
  RFC 3501 要求的子串匹配，但整个 Message-ID（带尖括号）能搜到，现有的做法可用。
- **已发送**：Gmail 自动把经它的 SMTP 发出的邮件存进已发送，再 APPEND 会多一份。Gmail 账户经同一个登录、
  同一个域的 SMTP 发出的跳过这一步；身份经别的服务器发出的 Gmail 看不到，照常保存（见
  [发信 / 保存到 Sent](sending.md#保存到-sent)）。

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

2026-10-25 同步接上之后（#127）用同一个账户手动验证（约 2750 封）：

| 行为 | 结果 |
|---|---|
| 首次同步 | 模式成为 Gmail，只 EXAMINE 三个位置；一封邮件一行（行数 = 三个位置之和，`X-GM-MSGID` 不重复），标签的计数与网页版一致 |
| 网页上移到已删除再移回 | 两次同步之间做完时，下次同步在"所有邮件"里以新 UID 看到同一个 `X-GM-MSGID`，原来那一行（行 id 不变）搬过去，下载过的正文仍在，不重新拉取 |
| 网页上只加 / 去标签 | 随 `CHANGEDSINCE` 拉到，报告为 updated，标签列表随之变化 |
| 后台同步 | GUI 开着时 INBOX 实时出现新邮件 |
| 在 Gmail 设置里隐藏"所有邮件" | 模式变为普通，"所有邮件"这一行删除，各标签成为普通文件夹、各存一份，计数与标签模式下相同；重新显示后回到 Gmail 模式，一封一行，计数复原 |

没有验证的：标签邮箱里列不列在已删除、垃圾邮件里的邮件（我们按 Gmail 网页版，不列）；经 SMTP 发出的
邮件自动进已发送（Gmail 的公开行为，未在本账户上发信）；非默认的删除设置
（"移到已删除"、"立即永久删除"）下标签邮箱里 `\Deleted` 的效果——我们不在标签邮箱里 STORE `\Deleted`，
不依赖它。

## 迁移步骤

1. ~~记下决定（本文、[IMAP 库](protocol.md#imap-库)、AGENTS.md）。~~
2. ~~用真实 Gmail 验证上面几点，同时试用 imap-proto。~~
3. ~~协议层改用 imap-proto，`Session` 的接口不变，现有的假服务器和 Dovecot 测试做回归。~~ imap-proto 暂时来自
   main，0.18.0 发版后换成 crates.io 的版本（见 [IMAP 库](protocol.md#imap-库)）。
4. ~~存储与同步：`folders.kind` / `listed`、`message_labels`、`gm_msgid` / `gm_thrid`、账户的模式；协议层的
   `X-GM-*`；假服务器支持 `X-GM-EXT-1`（#126）；按三个位置同步（#127）。~~ CI 里没有真的 Gmail，与真实
   服务器的行为只能手动验证。
5. ~~操作队列：标签操作、动作到操作的映射；删除按 Message-ID 找副本的机制（#128）。~~
6. GUI：侧栏分开位置和标签，显示邮件的标签。
