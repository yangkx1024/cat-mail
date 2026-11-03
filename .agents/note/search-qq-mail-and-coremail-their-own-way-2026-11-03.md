# 问服务器的搜索按 QQ 邮箱、Coremail 的实际行为来问

- 日期：2026-11-03
- 类型：修复

## 背景

#222 在真实服务器上验证问服务器的搜索（#196 / PR #220）。日志显示 QQ 账户每次都是 `matched=271 found=271`，
不管搜什么词：搜一个不存在的 `zzqqxx9`，GUI 里也冒出已删除邮件里的全部 271 封。用只读命令（EXAMINE、
`UID SEARCH`）直接问服务器发现：

- QQ 邮箱无视 `TEXT`：`TEXT x`、`NOT TEXT x` 都等于 `ALL`；`BODY`、`SUBJECT`、`FROM` 正常。它也不读 SEARCH
  里的字面量，把 `{n}` 本身当成要搜的文字（两个 9 字节的词命中完全相同，直接搜 `"{9}"` 也一样），而 QQ 没有
  LITERAL+，中文词都走同步字面量，结果和词无关。带引号的 8 位 UTF-8 能正常按子串搜中文。QQ 也无视带文字的
  `NOT`（不管怎么写都返回全部）。
- 163（Coremail）除 `HEADER Message-ID` 外，所有文字条件一律回空结果，不报错。

## 决策

- **按问候语认服务器**（`TextSearch::of_greeting`）：QQ（含企业邮）以 `QQMail ` 开头，Coremail 以 `Coremail ` 开头。QQ 的 ID
  回复是 NIL，用不上；问候语两家都有，也不用登录。STARTTLS 之前的问候语可能被篡改，但只影响搜索的写法。
- **QQ 不用 `TEXT`，改用 `OR BODY … OR SUBJECT … OR FROM … OR TO … CC …`**（本地搜索的几列），非 ASCII 文字用
  带引号的 8 位字符串（`Command::quoted_8bit`，不合 RFC 3501，只对 QQ）。考虑过所有服务器都改用这种 OR：
  不用认服务器，但没有 LITERAL+ 的服务器上每个字面量多一个来回，一个词要发五次；既然 QQ 的字面量问题本来就要
  认出它，`TEXT` 留给守规矩的服务器。
- **Coremail 不问**：`TextSearch::Unsupported`，`uid_search_text` 不发命令直接返回 `None`（第一个文件夹 EXAMINE
  之后），`SearchSlot` 记下，引擎运行期间不再连接去问（每次都要登录，163 上登录要
  2～4 秒）。按 Message-ID 找邮件的发送和草稿不受影响。
- **兜底**：一个文件夹全部命中时，用同样的写法（同样的字段、同样带不带 CHARSET）再搜一次随机文字（UUID），
  也有命中就当作服务器无视文字，返回 `None`，这个账户不再问。起初用"把条件取反再搜"，在 QQ 上实测发现它连
  `NOT` 也无视，会把正常的全部命中误判，改成随机文字。一条连接上通过一次就不再查（`Session.heeds_text`）：
  一两封邮件的小文件夹常常全部命中，Gmail 上每次多一个约 380 ms 的来回。
- `uid_search_text` 返回 `Option`，而不是 `Error::Unsupported`：后者是致命错误，连接会被丢掉。

## 方案

- `catmail-proto`：公开 `TextSearch`（`Keys`、`GmRaw`、`QqMail`、`Unsupported`），`Session` 记下问候语对应的
  那个，`Session::text_search` 在 Gmail 上换成 `GmRaw`；`uid_search_text` 按它写命令，`Session::heeds_text` 做
  全部命中的检查；`Command::quoted_8bit`。
- `catmail-core`：`search.rs` 遇到不能按文字搜的服务器停下、记进 `SearchSlot.unsearchable` 并关掉连接；日志的
  `syntax` 是 `TextSearch` 的名字。
- `catmail-testkit`：故障 `qq_mail`（问候语、`TEXT` 返回全部、SEARCH 里的字面量当 `{n}`）、`coremail`（问候语）、
  `search_ignores_text`；`BODY` 只匹配头之后。
- 文档：`storage.md` 问服务器一节、`logging.md`、`testing.md`。

验证：proto 单测（QQ 的命令写法、问候语识别、带 CAPABILITY 代码的问候语文字、`quoted_8bit`）；core 集成测试
4 个（QQ 不发 `TEXT` 且中文搜得到、Coremail 不 SEARCH、只开第一个文件夹且只连一次、无视文字的服务器什么都不补且
不再问、正常的全部命中照常并发出随机文字的对照）。逐个撤掉修复确认对应测试失败。新写法在真实 QQ 上用只读
命令确认：`账单` 5 封、`InfoQ` 60、`zzqqxx9` 和随机生僻字 0，两个词 AND 正常。iCloud 也用只读命令验证过：
标准，走 `TEXT` 不用特别处理，只是英文整词匹配（`a.b.c` 算一个词）。

- **/simplify 整理**（复用、简化、效率、层次四个角度）：
  - 去掉私有的 `Software`：它和 `TextSearch` 一一对应，问候语直接映射成 `TextSearch`；去掉只为日志用的
    `TextSearch::name`，日志用 `Debug`。
  - core 不再在 EXAMINE 之前另查 `Unsupported`，只认 `uid_search_text` 的 `None`：多一次 EXAMINE，但每次引擎
    运行只有一次。
  - 对照搜索每条连接只做一次（见上）。
  - 假服务器的 `coremail` 只改问候语：客户端不会对它发 SEARCH，原先穿过 `SearchKey::matches` 的参数没人用；
    头部结束的位置抽成 `header_end`，`BODY` 和 `filter_headers` 共用。
  - 没采纳：在登录前就认出 Coremail，省掉那一次登录（要改连接池和同步共用的连接，每次引擎运行只有一次；另开 #223）；
    把 `qq_mail` 拆成问候语、忽略的键、字面量三个独立故障（现在的故障都按真实服务器的表现来命名，没有测试
    需要单独开其中一个）。
- **codex review 修复**：对照搜索原先在 `uid_search_text` 里紧跟第一条 SEARCH，第一条执行期间被放弃又全部命中
  时会再扫一遍文件夹，下一次搜索要等它（`in_account` 一直拿着账户的锁），破坏了"做完手上的命令就停"。拆成
  `uid_search_text` 和 `heeds_text` 两步，core 在两步之间看一次是否放弃（新测试
  `a_search_given_up_on_doesnt_make_sure_of_everything_matching`，去掉这次检查即失败）。判断仍在 proto 里，
  core 只决定要不要接着问。
