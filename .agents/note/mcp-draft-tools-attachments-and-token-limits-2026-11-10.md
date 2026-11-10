# MCP 的草稿工具、附件以内容传入传出、每个令牌的限额

- 日期：2026-11-10
- 类型：功能

## 背景

MCP 第 3 步（`docs/architecture/mcp.md`）的后半。前半做了令牌的层级、账户范围和整理的工具；还差让 agent 写草稿
（新信、回复、转发、改草稿与附件），附件要以内容传（agent 碰不到常驻设备上的文件），以及每个令牌的限额：
agent 大量下载正文可能用光服务器给账户的下载额度（Gmail 超了封一天），GUI 也跟着受影响。

## 决策

- **新层级 `draft`**，在 `organize` 之上，`Tier::ALL` 多一项，CLI 的 `--tier draft`、GUI 的层级菜单随之多一项
  （"Read, organize and write drafts"）。工具：`create_draft`、`reply`、`forward`、`update_draft`、`attach`、
  `remove_attachment`；只读层加 `read_attachment`（附件传出）。
- **不提供删除草稿**：草稿可能是用户自己写的，删了找不回来，和"不永久删除"一致；agent 写坏了可以改。
- **`from` 写地址**：`list_accounts` 的 `sends_as` 就是可选值，按 `identity_by_address` 对到身份；身份 id 对 agent
  没有别的用处。`update_draft` 只在草稿自己的账户里找身份，不把草稿搬到别的账户（否则还要查令牌是否管那个账户）。
- **`update_draft` 只改给了的字段**：什么都没给是 `invalid`。放弃的方案：要求整份内容，agent 为改个主题得把正文
  （可能带长长的引用）原样送回来。
- **合并放进引擎（`DraftChanges`）**：起初工具先读草稿、换上给了的字段、整份写回（CLI 的 `draft edit` 原来也
  这样）。读和写之间用户在编辑器里存下的正文会被 agent 读到的旧正文覆盖，丢了用户打的字。现在
  `MailClient::update_draft(id, DraftChanges)`，每个字段 `Option`，`drafts::update` 在存它的写事务里读出草稿再换上；
  编辑器（GUI、frontend）传 `DraftChanges::all(from, content)`。放弃的方案：另加一个只改部分的方法、保留整份的
  `update_draft`：两个几乎一样的入口，整份的那个正是不该给只改一部分的调用方用的。
- **编辑器开着时锁住草稿**：剩下的两个问题，编辑器下一次保存把 agent 的修改写回旧值，以及 agent `attach` 的附件
  不显示在编辑器里、却随信发出，都因为编辑器不跟随草稿的变化。没让它跟随、合并（正在输入的正文框、两边改同一
  字段、附件列表、发送前的空隙，几百行，还要在两个平台手动试），而是编辑器开着时别人改不了：
  `MailClient::edit_draft` 在一个写事务里记下并读出草稿，返回 `DraftEditor`，编辑器的保存和附件经它做；
  `update_draft`、`attach_file`、`remove_draft_attachment` 在写事务里查 `Engine::editing`，记着就 `Conflict`。能和
  编辑器同时改的只有同一进程里的 MCP（一个数据目录一个引擎），所以在内存里记就够。按次数记（关窗与再开交错）。
  锁与写的权利分开：`EditMark` 不可复制，窗口的主人拿着、丢掉即放开（GUI 外壳放在 `COMPOSERS` 里，窗口移走就丢，
  不靠视图线程何时结束）；`DraftEditor` 可复制，视图用，它的修改带上自己的草稿 id（`editor: Option<DraftId>`），
  只能越过自己那份草稿的锁。起初是一个可复制的 `DraftEditor` 兼做锁，要 `released` 标记、`close()` 和外壳里另一个
  Drop 包装，`/simplify` 后拆开。没把删除、发送也锁上：在邮箱里删开着的草稿是用户自己的操作。放弃的方案：让 MCP 先查"是否开着"再改：查和改之间编辑器可能刚打开、读到旧的。
- **引擎加 `NewFile`（路径或内容）**，`create_draft`、`reply`、`forward` 的 `files` 与 `attach_file` 都收它；
  `Error::Attach` 的 `path` 换成 `file`（路径或名字的文字）。放弃的方案：另加 `attach_content`，MCP 先建草稿
  再逐个附加：一个附件太大时会留下半个草稿，和 GUI、CLI"附件不行就不建"的保证不一致。内容的名字要是
  `safe_filename` 原样留下的（没有目录、方向字符等，`invoice\u{202e}fdp.exe` 这类伪装进不来），否则拒绝；类型按
  扩展名猜，和路径一样。
- **附件大小**：一次调用给的文件解码后合计 10 MB，草稿层级令牌的 HTTP 请求体上限因此是 16 MB，其他令牌仍是
  1 MB（`http_body_util::Limited`，先验令牌再读请求体）：只有草稿的工具收文件，读、整理的令牌不该让宿主收那么多。
  按工具分做不到，工具名在请求体里。一封信的总量仍由引擎按服务器的 `SIZE`、`APPENDLIMIT` 限。
- **`read_attachment` 按 MCP 的内容类型给**，不给结构化输出：文本给文字（按字数截断），PNG/JPEG/GIF/WebP 给图片
  （模型能直接看），其余给 base64 的 blob 资源，前面一行说明是什么、内容是寄件人的。快照测试原先要求每个工具都有
  `outputSchema`，改为除它以外。
- **`DraftOut` 改了**：附件从文件名列表变成带 id 的对象（`remove_attachment` 要 id），并加上 `from`。
- **日志不记写的内容**：字符串只留 `from`、`to`、`cc`、`bcc`、文件名，其余换成字数（`tools::logged`）。按留下的
  写而不是按要藏的写，以后的工具加了带内容的字段，缺省也不进日志。
- **限额在内存里、按令牌名、对所有令牌一样**：每分钟 60 次调用（令牌桶，可一下子用完）；每天 250 MB 从服务器
  下载（Gmail IMAP 每天约 2500 MB 的十分之一），从第一次下载起 24 小时。只算为这个令牌的调用下载的、本地还没有的
  正文和附件（按 `RFC822.SIZE` 或附件的大小，下载前扣，失败只退回还没下载的）；预取过、读过的不算。放弃的方案：存进令牌
  文件、可逐个设置：现在没人需要，CLI 和 GUI 还得各加一套设置；`ServeOptions::limits` 留着，测试用，以后要设置
  再接上。超了作为工具的错误（`over_limit: …`）给 agent 读，不回 HTTP 429：客户端常把 429 当成服务器出错，模型
  看不到该等多久。
- **Codex 审查后的两处修正**：一是回复、转发在下载原信之后才可能失败（文件名不对、超过大小），原先失败就全额退回，
  换着信反复调用就能绕过每天的预算白下载，之后 `read_message` 读已在本地的也不扣；现在失败时重新读这封信，只退回
  还没下载的部分（`Mail::left_of`）。没选"先单独下载再建草稿"：转发大邮件时正文和附件分开扣会重复计算。二是
  `/simplify` 时让 `read_message` 在正文已在本地时不经 `download_message`，跳过了它对旧版本正文的重建
  （`rebuild::on_open`），后台重建完成前 agent 读到旧的解析结果；改回总经过引擎，费用为 0。

## 方案

- `catmail-types`：`NewFile`、`DraftChanges`。
- `catmail-core`：`drafts.rs` 的 `copy_file` 分成 `copy_path`、`store_content`，`file_name` 校验名字；
  `MailClient::create_draft`、`reply`、`forward`、`attach_file` 收 `NewFile`；`Error::Attach { file }`；
  `update_draft` 收 `DraftChanges`，合并在 `drafts::update` 的写事务里。CLI 的 `--attach` 包成 `NewFile::Path`，
  `draft edit` 只交给出的字段（`Fields::changes`）；GUI、frontend 的编辑器用 `DraftChanges::all`。
  `MailClient::edit_draft`、`DraftEditor`、`EditMark`、`Engine::editing`（`drafts::not_open_elsewhere`，返回 store
  的错误，被拒的移除附件随之回滚）。frontend 的 `Opening::editor`、`Steps::run` 经它保存，`ComposeFrom::start`
  另返回 `EditMark`；GUI 的 `Composing::editor`，外壳把 `EditMark` 放在 `COMPOSERS` 里。
- `catmail-mcp`：`limits.rs`（`TokenLimits`、`Limits`）；`tokens.rs` 的 `Tier::Draft`；`http.rs` 的请求体上限与
  `ServeOptions::limits`；`tools.rs` 的新工具、`call_tool` 里的调用频率与 `caller`、`downloading`（扣预算）、
  `logged`；`output.rs` 的 `DraftOut`、`DraftAttachmentOut`；快照重新生成。
- `catmail-frontend`：`tier_label`、`serve_options` 用 `..ServeOptions::default()`。
- 文档：`mcp.md`（工具表、"草稿"、"限额"、日志、第 3 步打勾）、`cli.md`、README、`architecture.md`。
- `/simplify` 之后：`file_name` 复用 `safe_filename`；限额与大小的说法用 `display::size`；截断与 `read_message`
  共用 `output::cut`；`Recipients`、`AttachParams` 用 `#[serde(flatten)]`（schema 不变）；`read_message` 只在没有
  正文时下载，不再多读一次。留下没做的：下载的花费按 `RFC822.SIZE` 估，大邮件（引擎只取文本部分）会多扣，要准
  得让引擎报告实际收到的字节；"读、合并、写"在之后改成了引擎的
  `DraftChanges`（见上）。
- 验证：core 新测试 `attachments_can_be_given_by_their_content`（名字去空白、类型、坏名字（含方向字符）在附加和新建时都拒绝、
  超限、发出去的信里附件内容对）；`catmail-mcp` 经 HTTP 的测试：写草稿（地址、Bcc、base64 附件）、只改给了的字段、
  附加与移除、各种拒绝都不留草稿；回复、转发（带原信附件和新给的附件）、读文本与二进制附件；`organize` 令牌看不到也
  调不了草稿工具；限定到 alice 的令牌对 bob 的邮件、草稿、账户都得 `not_found`；调用频率超了报 `over_limit`、别的
  令牌不受影响；下载预算放得下第一封放不下第二封，读过的再读不扣，第二封没被下载；日志里正文、主题、附件内容只剩字数；只读令牌发 1.5 MB 的请求被拒、草稿令牌的照常；回复因坏文件名失败后原信的下载仍扣着（`a_draft_failing_after_its_download_keeps_it_charged`）（去掉修正会失败）。读正文总经过引擎这一处没有 MCP 的测试：要让正文看起来像旧版本存的，测试得绕过 `MailClient` 直接改数据库，MCP 就要依赖 SQLite、写死表结构；重建本身由 core 的 `bodies.rs` 测，`Mail::with_body` 的注释说明为什么不能只读本地的。
  core 的 `a_draft_open_in_an_editor_is_changed_only_by_it`（开着时别处的三种修改都 `Conflict`、被拒的移除已回滚、
  编辑器自己的照常、编辑器动不了别的开着的草稿的附件、两个标记都丢了才放开）与 MCP 的 `a_draft_open_in_the_app_is_left_alone`；
  `a_change_to_some_parts_keeps_what_was_saved_meanwhile`：读出草稿后编辑器存了新正文，再只改主题，
  正文仍是编辑器存的；坏地址整个拒绝、什么都不改。`limits.rs` 的单元测试用传入的时间测令牌桶与 24 小时的预算。没有 Nix，用系统的 cargo；`instance` 的测试要 Unix
  socket，在沙箱外跑。GUI 层级菜单里的新项、写信窗口开着时 agent 被拒都没有手动看过。
