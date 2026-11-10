# MCP 令牌的层级与账户范围，以及整理邮件的工具

- 日期：2026-11-10
- 类型：功能

## 背景

MCP 第 3 步（`docs/architecture/mcp.md`）的前半。第 1、2 步之后所有令牌一样：只读、能看所有账户。要让 agent
整理邮件，先得能按令牌给不同的权限，并把令牌限定在部分账户上。草稿、附件和每个令牌的限额留到下一步。

## 决策

- **层级只做到"整理"**：`Tier` 现在只有 `read`、`organize`，草稿、发信等它们的工具有了再加。先加进枚举的话，
  用户能建出"能发信"的令牌，实际却什么也多做不了。文件里按字符串存，以后加层级不影响旧文件。
- **令牌文件不升版本**：新字段 `tier`（缺省 `read`）和 `accounts`（缺省所有账户）都有缺省值，旧条目读成只读、
  所有账户。项目还没发布，和迁移改 `0001_init.sql` 一个道理。
- **账户按地址记，不按 id**：地址是用户认得、CLI 里写得出的；账户删掉再加回来还是同一个范围。`token add` 不打开
  引擎，所以只查形如地址，不查是否存在；对不上的地址就是什么也看不到（失败时关闭）。
- **令牌跟着请求进工具**：`http.rs` 把验过的 `Token` 放进 HTTP 请求的 extensions，rmcp 会把请求的 parts 放进
  `RequestContext`。覆盖 `list_tools`（按层级筛，`cache_scope` 改为 `Private`：列表因令牌而异）和 `call_tool`
  （查层级、把地址对成当时的账户 id，交给这次调用的 `Mail` 副本）。放弃的方案：每个工具自己取令牌，容易漏。
- **工具的层级逐个写死（`tier_of`），没有缺省**：新工具不写层级就谁也看不到；快照测试检查每个工具都有层级。不按
  `readOnlyHint` 推断：`sync` 不是只读，却属于只读层级。
- **范围外即不存在**：账户、文件夹、邮件（分页的 `after` 也算）不在范围内时报 `not_found`，和真的不存在没有区别。
  合并的收件箱和搜索要在数据库里筛，否则一页筛掉一部分，分页就乱了：`MailClient::list_inboxes` 加了 `accounts`
  参数，`SearchQuery` 的 `account` 换成 `accounts`（`None` 不限；空列表就是一封也没有，而不是全部），服务器搜索
  （`searched_on_server`）也按它筛账户。放弃的方案：另加 `list_inboxes_of`、让 `account` 与 `accounts` 并存取交集，
  两套几乎一样的接口，交集的规则每个调用方都得想一遍。
- **`trash` 绝不永久删除**：原来的 `DeleteMode::ToTrash` 对已在废纸篓的邮件是永久删除。加了
  `DeleteMode::OnlyToTrash`，在同一个写事务里判断，有一封在废纸篓或账户没有废纸篓就整个拒绝。放弃的方案：工具先
  查位置再删，两步之间同步可能把邮件移进废纸篓。
- **改动之后立即回放并报告结果**：对涉及的账户 `replay_pending`，最多等 20 秒（回放先等进行中的同步，首次同步
  可能很久；放弃等待不会取消回放，`Runtime::run` 的任务不会被中止），然后列出这次改动中仍在队列里、被拒绝或成了
  冲突的操作。报的是队列状态而不是这次回放的计数：常驻引擎自己也会回放，这次可能什么也没轮到。
- **按操作 id 认这次改动，不按邮件 id**（Codex review 指出）：起初按邮件 id 筛 `list_pending`。源文件夹的
  UIDVALIDITY 变了时，回放把移动变成冲突，`drop_moving_rows` 删掉本地移过去的那一行，`message_id` 被
  `ON DELETE SET NULL` 清空，冲突就被筛掉了：`archive` 答 `not_on_server: []`，邮件却还在 INBOX。现在 store 在
  `Touched::ops` 里记下改动排进队列或改了去向的操作，五个改动邮件的 `MailClient` 方法返回 `ChangeReport`
  （账户与操作 id），GUI、CLI 只取账户。放弃的方案：改完再按邮件 id 查一次操作 id，常驻引擎的后台回放可能抢先。
- **日志**：不是只读的工具每次调用记一行 info（令牌名、工具、参数 JSON、结果）。现在参数只有 id；草稿的工具带正文，
  到时要挑字段。
- **GUI 只能选所有账户或其中一个**：名字下面两个菜单（层级、账户），加好后回到缺省（只读、所有账户），下一个令牌
  不会沿用上一个的权限。多选账户只在 CLI（`--account` 可重复）：一排勾选框对只有一个账户的人只是噪音。
  文字大小那个"显示当前值、按下弹出菜单"的控件抽成 `form::menu_field`，三处共用。

## 方案

- `catmail-types`：`DeleteMode::OnlyToTrash`；`SearchQuery::account` 换成 `accounts`。
- `catmail-store`：`delete_messages` 处理 `OnlyToTrash`；`search` 按账户集合筛；`messages().summaries(ids)`。
- `catmail-core`：`MailClient::list_inboxes` 的 `accounts` 参数、`message_summaries`；服务器搜索按账户集合筛。
  CLI 的 `search` 与 dovecot 测试随之改用 `accounts`。
- `catmail-mcp`：`tokens.rs` 的 `Tier`、`Access`（`allows`、校验地址、去重）；`http.rs` 把令牌放进请求；
  `tools.rs` 的 `tier_of`、`Scope`、`list_tools` / `call_tool`、新工具 `mark`、`move`、`archive`、`label`、`trash`
  共用的 `change`（查邮件、做改动、回放、报告）、列出所有账户时共用的 `narrowed`；`output.rs` 的 `Changed`、
  `AccountError`；快照重新生成。
- `catmail-cli`：`mcp token add --tier --account`；`list` 加 TIER、ACCOUNTS 两列；JSON 加 `tier`、`accounts`。
- `catmail-frontend`：`mcp::tier_label`、`accounts_label`、`token_line`。
- `catmail-gui`：`settings/agents.rs` 的两个菜单与每行的说明；`form::menu_field`。
- 文档：`mcp.md`（令牌、工具与权限、步骤）、`cli.md`、README、`architecture.md`、ROADMAP。
- 验证：store 测试（账户集合筛选、`summaries`、`OnlyToTrash` 不永久删除且整体拒绝）；`catmail-mcp` 经 HTTP 对
  假 IMAP 服务器的测试：只读令牌看不到也调不了整理的工具；限定到 alice 的令牌在 bob 的账户、文件夹、邮件、分页
  游标上都得 `not_found`，合并收件箱和搜索只有 alice 的；`mark` 后服务器上有 `\Seen`、`\Flagged`；`archive`、
  `move` 到废纸篓、`trash` 后服务器上的位置，以及废纸篓里的再 `trash` 被拒绝；服务器拒绝的改动出现在
  `not_on_server` 且本地已撤销；源文件夹 UIDVALIDITY 变了时 `archive` 报出 `conflict`（修之前答
  `not_on_server: []`）；Gmail 上 `label` 加标签。store 另测改动记下的操作 id：移动途中改去向、再移到废纸篓都是同
  一个操作，成了冲突、行被删掉后操作仍在。令牌文件：层级与账户的读写、旧条目读成只读、地址校验。
  CLI、frontend 各加文字的测试。没有 Nix，用系统的 cargo；`instance` 的测试要 Unix socket，在沙箱外跑。GUI 的
  两个菜单没有手动看过。
