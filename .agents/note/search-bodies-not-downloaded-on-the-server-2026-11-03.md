# 搜索时问服务器，补上正文没下载的邮件

- 日期：2026-11-03
- 类型：功能

## 背景

后台预取（#195）受每日预算限制，更早的邮件仍然没有正文，按正文搜不到（#196）。服务器上有全文，搜索时再问一次，
把找到的邮件补进结果。

## 决策

- **服务器的命中当过滤条件，不当另一批结果。** `MailClient::search_server` 只返回服务器找到、正文还没下载的邮件
  id；前端放进 `SearchQuery.found_on_server`，store 的搜索与 FTS 取并集。考虑过返回补充的摘要、由前端合并：
  补充的邮件日期散在各页，合进已显示的页会打乱顺序和下一页的游标；`MessagesChanged` 来了重读本地结果，补充的又
  丢了。现在排序、分页、事件后重读走同一条 SQL，引擎不缓存搜索结果。也考虑过只把命中交给预取下载正文、让本地
  FTS 自己找到：慢、受预算限制、一次性的 CLI 要等，暂不做。
- **全部文件夹都搜**（用户选定），范围与本地搜索相同：普通账户每个可选位置（不含只汇集其他文件夹的 `\All`、
  `\Flagged`，新视图 `gathering`，本地搜索的 `SEARCHED` 也改用它），Gmail 三个位置；指定文件夹只搜它，Gmail 标签
  搜"所有邮件"。没有正文缺失的邮件的文件夹不问。没设总时限：本地结果先显示，不等它。
- **每个账户一条搜索专用连接**，不用连接池（两条留给打开邮件），空闲 2 分钟关闭。调用方丢弃 future 即放弃
  搜索：引擎做完手上的命令就停并把连接让出来，而不是中途丢弃连接，免得每次停顿输入都重新登录。起初用引擎里的
  "最新搜索"计数让新搜索作废旧的，/simplify 时改掉：同一引擎的多个调用方会互相取消，还占用了 `Error::Cancelled`。
- **服务器不支持不算错**：SEARCH 被拒（`BADCHARSET` / NO / BAD）就停这个账户；连不上只记日志；登录被拒或超限的
  账户不问。返回值只说"多了哪些"，没有失败列表：GUI 也只会忽略。
- 普通 IMAP 每词一个键（`TEXT`、`FROM`、`OR TO CC`、`SUBJECT`），Gmail 用 `X-GM-RAW` 写成它的语法；非 ASCII 加
  `CHARSET UTF-8`、用字面量。查询解析从 store 移到 `catmail-types`（`SearchTerm::parse`），本地和服务器共用。
- 每个文件夹最多取正文没下载的里 UID 最高的 1000 封，免得一个常见词让每次读结果都带上几万个 id。

## 方案

- `catmail-types`：`SearchTerm` / `SearchField` 与解析，`SearchQuery.found_on_server`。
- `catmail-store`：`fts.rs` 用共用解析；`Messages::search` 并入 `found_on_server`；`Messages::without_body`
  （UID → 同一 UIDVALIDITY 下正文没下载的行）；`Folders::searched_on_server`（要搜的位置）。
- `catmail-proto`：`Session::uid_search_text` 与命令编码。
- `catmail-core`：新模块 `search.rs`（各账户并行、逐个文件夹 EXAMINE + SEARCH、放弃、空闲关闭），`Engine` 加
  每账户的 `SearchSlot`，账户删除时关掉；`MailClient::search_server`。
- CLI：`catmail search --server`。GUI：`MessageList` 的本地结果显示 1 秒后问服务器（`SERVER_SEARCH_DELAY`：
  放弃的搜索仍要做完手上的命令，没有索引的服务器上可能要几秒，下一次要等它；所以输入中 200 ms 的停顿不去问，
  服务器结果晚到约 1 秒），有结果就重读一次，之后翻页和重读都带上。
- `catmail-testkit`：`UID SEARCH` 支持 `TEXT`/`BODY`、`FROM`/`TO`/`CC`/`SUBJECT`、`OR`、`CHARSET`、`X-GM-RAW`；新故障
  `refuse_search_charset`。
- 文档：`storage.md` 新增"问服务器"一节，`gui.md`、`testing.md`、README 相应修改。

验证：`cargo fmt`、`clippy -D warnings`、全部测试通过。新增 types 单测 1 个（解析）、proto 单测 2 个（普通 IMAP 与
Gmail 的命令）、store 测试 1 个（UID 映射、并集与范围、要搜的文件夹）、core 集成测试 8 个（找到本地搜不到的且只用
EXAMINE、中文带 CHARSET、正文已下载的不再返回、只搜指定文件夹、没同步到的忽略、BADCHARSET 后不再搜且 ASCII 照常、
连不上时为空、几次搜索共用一条自己的连接且不占打开邮件的、放弃的搜索停下并把连接让给下一次、Gmail 的位置与 X-GM-RAW 及标签范围）、
GUI 测试 1 个（搜索结果里出现服务器找到的邮件，去掉传入 `found_on_server` 会失败）。本地对 Dovecot 2.4 跑了
`dovecot`、`submission` 测试，新增的 `bodies_not_downloaded_are_searched_on_dovecot` 确认没有 FTS 插件的 Dovecot
能按子串搜到 UTF-8 和 GBK（base64）的中文正文，并搜过 UTF-7 名字的文件夹。没有对真实 Gmail、QQ、网易试过：它们是否
支持 `CHARSET UTF-8`、Gmail 对中文短语的匹配还待验证。
- **/simplify 整理**（复用、简化、效率、层次四个角度）：
  - 搜索范围规则原先在 `SEARCHED` 和 `searched_on_server` 里各写一遍，抽成视图 `gathering`；账户范围的解析从 core
    移进 `searched_on_server`（返回按账户分组的位置）。
  - "词里有没有可搜的内容"原先在 types、core、proto 各判断一次：`SearchTerm::parse` 直接丢掉这样的词，去掉
    `SearchQuery::terms()`；proto 不再过滤。
  - 没有正文缺失的邮件的文件夹不再去问服务器。
  - 换成调用方丢弃即放弃（见上），去掉 `Engine::searches`；UIDVALIDITY 检查改用 `MailboxStatus::same_generation`；
    `in_folders` 不再返回只用于日志的计数。
  - 假服务器：解析失败用 `Option` 表示，错误分支移到 match 之前，`*` 代表的最大 UID 每条命令只算一次；GUI 测试
    复用 `deliver_raw_and_sync`（加了指定邮箱的版本），不另加投递函数。
  - GUI 本地结果显示 1 秒后才问服务器（见上；与作者确认后加的），GUI 测试推进时钟，确认改字期间不问、只问最后
    的文字。
  - 没采纳：EXAMINE 与 SEARCH 流水线发送（`Session` 没有一次发多条命令的支持，另开 #219）；每个文件夹一次数据库读合成一次（微秒级，对比两次网络来回）；测试里的 `log_len` /
    `connections` 小函数挪进 `common`（别的测试文件也各自内联）。
- **codex review 修复**（3 条，均先写出不修就失败的测试或变异确认）：
  - 1000 封的上限原先在过滤正文已下载的邮件之前截断：最新的 1000 封正文都在时，要补的旧邮件被截掉。改成
    `Messages::without_body` 带上限，先过滤再按 UID 取最高的（store 测试：较新的那封有正文时，上限 1 留下较旧的）。
  - 搜索自己遇到的登录被拒、超限只记日志，`waits_out` 只看同步记下的：同步之后密码失效时，每次停顿输入都再登录
    一次。现在记在 `SearchSlot`，等之后一次完整同步成功才再问（`a_server_refusing_the_login_isnt_asked_again_until_a_sync_gets_through`，
    拒绝登录和 Gmail 超限两种）。
  - 放弃只在每个文件夹开始前检查，EXAMINE 期间放弃的仍会发 `UID SEARCH`。现在 EXAMINE 之后也看一次
    （放弃测试多一种：卡在 EXAMINE 时放弃，不发出正文搜索）。
- **日志**（为在真实服务器上排查）：每个账户一行 info 汇总服务器命中数（与补到的数分开）、用的语法、是否带
  `CHARSET UTF-8`、词数、EXAMINE 与 SEARCH 各自的总耗时，不记搜索词；每个文件夹的两段耗时在 debug；GUI 发起搜索
  失败时原先静默忽略，现在 warn。写进 `logging.md`。
