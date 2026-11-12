# MCP

[← 架构](../architecture.md)

> 状态：第 1 至 3 步与第 5 步的发信已实现：`catmail-mcp` 的只读、整理、草稿（附件以内容或经上传传入、以内容传出）与发信的工具、
> `catmail serve`、令牌文件（层级与账户范围）、每个令牌的限额、GUI 托管（Settings › Agents）、改动的日志。无界面
> 服务器的密码、OAuth 尚未实现，那些小节是设计。

把 catmail 的能力经 [MCP](https://modelcontextprotocol.io) 交给 AI agent：一台设备常驻，局域网和
tailnet 里的 agent 都能读信、整理、写草稿，经确认后发信。

## 宿主：谁持有引擎谁托管

MCP 不是第三个各自打开数据库的前端，而是一个库 `catmail-mcp`（只依赖 `catmail-core`），由持有引擎的进程
托管：

```
                 catmail-mcp（工具、权限、令牌、HTTP 服务端）
                 McpServer::start(client, tokens, options) -> McpServer { stop() }
                        ▲                                ▲
          catmail-frontend 的 mcp::Host            catmail serve（CLI，无界面常驻）
          专门的线程，自己的 tokio runtime        #[tokio::main] 的 runtime
                        ▲
          catmail-gui（Settings › Agents）
```

依赖方向 `core ← mcp ← cli, frontend ← gui`。

- **为什么不让 MCP 自己打开引擎**：一个数据目录一个引擎，GUI 常驻时 MCP 打不开；反过来 MCP 开着时 GUI 启动不了。
- **为什么不先做通用 IPC**：`MailClient` 有 86 个公开方法，事件、流（远程图片）、大块数据（附件）、同步方法
  （`account_status`）和不透明的错误都要过线，门面每改一次协议跟着改；项目发布前门面还在变。MCP 本身就是
  有版本的公开协议，托管在引擎所在的进程里不需要私有协议。
- 同一个数据目录只会有一个宿主，引擎的锁保证。两个宿主的引擎配置相同（后台同步、预取正文、后台发信、
  GC）：`MailClientBuilder::resident()`，GUI 和 `serve` 都用它。
- GUI 里的服务跑在专门的线程上（`catmail_frontend::mcp::Host`），用它自己的 current-thread tokio runtime，
  不碰工具包的主循环；`MailClient` 的 future 不依赖 executor（见[运行时桥接](../architecture.md#运行时桥接)）。
  开关随时启停，不重启 GUI。详见下文 [GUI 托管](#gui-托管)。
- `catmail serve` 也是 Windows 上唯一的宿主（那里还没有 GUI）。

| | GUI | `catmail serve` |
|---|---|---|
| 开关、端口 | Settings › Agents（存在 `settings.json`） | `--port` |
| 令牌 | Settings › Agents 里增删 | `catmail mcp token add / list / rm` |
| 开机自启 | 登录项与 `--background`（见[后台运行](background.md)） | systemd 用户服务、launchd |
| 添加账户 | 设置 | 先停掉 `serve`，`catmail account add` |
| 密码 | 系统密钥环 | 密钥环；无 Secret Service 时见[无界面服务器的密码](#无界面服务器的密码) |

## GUI 托管

Settings › Agents：开关、端口，下面是令牌。

- **服务的线程**（`catmail_frontend::mcp::Host`）：引擎打开后由外壳（`shell/mod.rs`）建好，一直到应用退出；
  服务按设置开、关、换端口。外壳像视图一样跟随设置，每变一次把**当时生效的**设置交给它（两个视图线程上的两次
  修改可能以任意顺序到达），`update_settings` 本身不认识 MCP。它只看最新的一个，与正在用的相同就不动：改别的设置不会重启服务，也不会重试刚失败的端口（关掉再
  打开才重试）。换端口时先停旧的再开新的。
- **状态**：没在监听、在监听（显示本机的地址，可选中复制）、失败（端口被占用、没有权限……一句话说明）。开关打开
  而还没在监听时显示"Starting…"（`Status::line`）。关掉时"没在监听"在端口真正释放之后才报告，换端口时先报告它
  再停旧的。状态由外壳转给正显示这一节的视图：读当前状态和开始跟随在同一把锁下（`Watched`，与设置共用），中间的变化不会丢。
- **停下等多久**：GUI 停服务时让正在答的请求最多再跑 5 秒（`serve` 是 30 秒）：有人等着应用。退出时先停服务、
  再关引擎，正在答的请求能答完而不是撞上已关闭的引擎。
- **端口**按 Return 或离开输入框时才生效，逐字生效会让服务在 8、86、864 上依次开一遍。
- **令牌**：列表在进入这一节时读（`catmail mcp token` 也会改它）；每行在名字下面写层级、账户和创建日期。新令牌
  显示在列表下面，只这一次，旁边是复制按钮；删掉前确认，删掉即失效。读写令牌文件在单独的线程上做，不卡视图。
- **新令牌的层级与账户**：名字下面两个菜单，层级（缺省"Read only"）和账户（缺省"All accounts"，或其中一个）。
  加好后两者回到缺省，下一个令牌不会沿用上一个的权限。限定到多个账户只在 CLI（`--account` 可重复）：GUI 里多选
  要一排勾选框，而一个账户的人最多，它们只是噪音。
- **Connect an agent**：把服务加进 Claude Code、Codex 的命令，地址跟着端口；刚加了令牌时填上它，可以直接粘贴，
  否则是 `<token>`。每段一个复制按钮。长命令用 `\` 在选项之间换行，窄的地方也不会从选项中间折开。
- **示例的文字**在 `catmail_mcp::connect`，GUI 和 `catmail mcp token add` 共用。Codex 只从环境变量读 Bearer
  令牌（`--bearer-token-env-var`，配置里不能写令牌本身），所以它的示例是两行：`codex mcp add` 和
  `export CATMAIL_MCP_TOKEN=…`，并说明要在 Codex 运行的环境里设好。
- 日志：GUI 的日志过滤加上 `catmail_mcp::LOG_FILTER`，监听、停止、被拒的令牌都记下来。

## 传输：只听本机，网络交给 Tailscale

只做 MCP 的 Streamable HTTP，只监听 `127.0.0.1`（`catmail serve` 默认端口 8642，路径 `/mcp`）。本机的 agent 直接连；其他设备经 Tailscale：
`tailscale serve` 用 tailnet 的证书（`*.ts.net`）提供 HTTPS 并转到本机端口，WireGuard 端到端加密，只有
tailnet 里的设备连得上，同一局域网内是直连。catmail 因此不做 TLS、不管证书、不挑网卡。

- 不做 stdio：常驻的宿主不是 agent 启动的子进程，本机也走 HTTP，只有一种传输。
- 不建会话（rmcp 的 `legacy_session_mode` 关掉）：每个请求自成一体，与 MCP 2026-07-28 去掉会话一致，重启或换一条
  隧道都不丢东西；答复是普通的 JSON（`json_response`），要推送通知时 rmcp 才改用 SSE。
- 底下是 hyper 的 HTTP/1.1，加 rmcp 的 `StreamableHttpService::handle`，没有 axum。请求体最多 1 MB。
- 停下时先不再接受连接，让正在答的请求（同步、下载正文）答完，最多等 30 秒（`ServeOptions::drain`），之后才取消
  rmcp 的 token：rmcp 一见它取消就放弃等答复、回 500，所以它不能是停止信号的子 token。到时还没结束的连接直接中止：
  请求体一直没发完的客户端，rmcp 的 token 和 hyper 的优雅关闭都等不到头。
- Cloudflare Tunnel 也能用，但服务到了公网，TLS 终止在 Cloudflare（邮件内容在那里是明文），应在前面加
  Cloudflare Access。
- 经隧道来的请求源地址都是 `127.0.0.1`，所以本机的请求同样要令牌（`http.rs`，在交给 rmcp 之前查）；带
  `Origin` 的请求一律拒绝（rmcp 查）：agent 不是网页，浏览器里的页面不该连上来。唯一的例外是上传地址
  `/mcp/uploads/<句柄>`：凭的是地址里的句柄而不是令牌，带 `Origin` 的同样拒绝（见[附件经上传传入](#草稿)）。
- **不查 `Host`**（rmcp 的 `disable_allowed_hosts`）。`Host` 白名单防的是 DNS rebinding：网页把自己的域名解析到
  127.0.0.1 来访问本机端口。这样的请求过不了上面两关：浏览器不会自动带 `Authorization`，网页也拿不到令牌；
  浏览器发 POST 总带 `Origin`，同源的也带（MCP 的请求都是 POST）。剩下能做到的只是看到 401，知道这里开着
  catmail。而查 `Host` 的代价是每个隧道的名字都要配置：`tailscale serve` 和 Cloudflare Tunnel 默认保留原来的
  `Host`（`*.ts.net`），漏配时被拒，报错又不指向这项配置。所以第 1 步的 `--host` 去掉了。
- claude.ai 网页、手机 App 的远程连接器需要 OAuth（或者不认证），带不了固定的请求头；要用它们时再做
  OAuth 2.1，在此之前只支持 Claude Code、Codex、Cursor 这类能配请求头的客户端。

## 令牌

每个 agent 一个 Bearer 令牌，各有权限层级和账户范围，可以单独吊销。

- **放在配置目录的单独文件**（`mcp-tokens.json`，0600），不进 SQLite：只存名字、SHA-256、创建时间、层级
  （`tier`，缺省 `read`）和账户（`accounts`，缺省即所有账户），明文（`catmail_` 加 64 位十六进制）只在创建时
  显示一次。没有 `tier` 的旧条目按只读读，文件版本不变。
- **账户按地址记，不按 id**：地址是用户给的名字，CLI 写 `--account a@example.com`；账户删了再加回来仍是同一个
  范围。`add` 不打开引擎，所以不检查地址现在是否存在（只查形如地址），对不上任何账户的令牌什么也看不到。
  每次调用时把地址对到当时的账户上，不区分大小写（与数据库的 `COLLATE NOCASE` 一致）。
  - 管令牌不用打开引擎：`serve` 运行时 `catmail mcp token add` 照样能用，不被引擎的锁挡住。
  - 服务端每个请求都重读（文件很小，在 blocking pool 上读），吊销立即生效，也不用担心修改时间的精度；写入
    在 `mcp-tokens.lock` 的文件锁下读、改，先写临时文件再改名，读到的总是完整的。
  - 同一台机器上换宿主（GUI 与 `serve`）令牌不变；GUI 写的也是这个文件。
- 每个令牌有调用频率和每日下载预算，见下文[限额](#限额)。

## 工具与权限

权限分五层，层层包含：只读 < 整理 < 草稿 < 发信（先问用户）< 不问就发信。令牌没有的层级，工具不出现在 `tools/list`
里，调用时得到与不存在的工具相同的错误（`tool not found`）。

- **每个工具的层级逐个写在 `tools::tier_of` 里**，不按注解推断，也没有缺省：新加的工具不写就哪个令牌也看不到、
  调不了，快照测试检查每个工具都有层级。
- **令牌跟着请求走**：`http.rs` 验过令牌后把它放进 HTTP 请求的 extensions，rmcp 把请求的 parts 交给处理函数。
  `call_tool` 在每次调用时把令牌的账户对成 id（`Scope`），交给这次调用的 `Mail` 副本，工具里不必再查令牌。
- **账户范围外的东西就是不存在的**：账户、文件夹、邮件（包括分页的 `after`）不在范围内时报 `not_found`，与真的
  不存在一样，agent 分不出来。列表（所有收件箱、搜索、草稿、发件箱、待同步的改动）只含范围内的账户：
  `MailClient::list_inboxes` 的 `accounts` 与 `SearchQuery::accounts` 在数据库里筛，分页不乱；服务器搜索也只问这些
  账户。列出所有账户的东西都经 `Mail::narrowed`（令牌的账户，再按 `account` 参数收窄），不另写筛选。

| 层级 | 工具 |
|---|---|
| 只读 | `list_accounts`、`list_folders`、`list_messages`、`search`（本地，可选再问服务器）、`read_message`（纯文本，按字数截断）、`read_attachment`、`list_drafts`、`list_outbox`、`list_pending`、`sync` |
| 整理 | `mark`（已读、星标）、`move`、`archive`、`label`（Gmail）、`trash`，之后立即回放队列 |
| 草稿 | `create_draft`、`reply`、`forward`、`update_draft`、`attach`、`remove_attachment`、`prepare_upload` |
| 发信、不问就发信 | `send_draft` |

不提供：永久删除、删除文件夹、删除草稿、增删账户和身份、`retry_outbox`、`mark_outbox_sent`（`unknown` 只由
用户处理，见[草稿与发信](sending.md)）。删除草稿也算永久删除：草稿可能是用户自己写的；agent 写坏了可以
`update_draft` 改掉。

- 读信用 EXAMINE 与 `BODY.PEEK`，agent 读过的信不会变成已读。
- 工具带 MCP 的 annotations（`readOnlyHint`、`destructiveHint`、`idempotentHint`），客户端据此决定要不要
  问用户。
- 输出是 `structuredContent` 与 `outputSchema`，另附文本；结构在 `catmail-mcp` 里逐个写出，不复用 CLI 的
  `json.rs`（agent 在意 token 数，两边的版本约定也不同），快照测试固定工具列表与 schema。
- 一次操作的邮件数有上限（100）。
- **整理的工具走操作队列**，与 GUI 相同：本地立即改，再回放到服务器。答复前对涉及的账户调 `replay_pending`，最多
  等 20 秒（回放要先等进行中的同步，首次同步可能很久），再列出这次改动中还没到服务器、被拒绝（已在本地撤销）或
  成了冲突的部分（`not_on_server`），以及连不上的账户（`errors`）。超时不算错误，改动照样在路上，显示为 pending；
  常驻的引擎自己也会回放，所以报的是队列状态，而不是这次回放做了几个。
- **按操作认，不按邮件认**：引擎的改动（`set_flags`、`move_messages` 等）返回 `ChangeReport`：涉及的账户和
  这次排进队列（或改了去向）的操作 id，在同一个写事务里记下。不能按邮件 id 找：文件夹的 UIDVALIDITY 变了时，移动
  成了冲突，本地移过去的那一行被删掉，操作的 `message_id` 随之成了 NULL（`ON DELETE SET NULL`）。也不在改完之后
  另查一次队列：常驻引擎的后台回放可能已经抢先把它变成冲突。
- **`trash` 只移到废纸篓，从不永久删除**：引擎的 `DeleteMode::OnlyToTrash`，有一封已在废纸篓或账户没有废纸篓
  就整个拒绝、一封也不动。在同一个事务里判断，不是工具先查再删：两步之间同步可能把邮件移进废纸篓。
- **改动的日志**：不是只读的工具（`readOnlyHint` 不为 true，`sync` 也算）每次调用记一行 info：令牌的名字、工具、
  参数、结果（`done` 或错误的文字）。字符串只留下 `from`、`to`、`cc`、`bcc` 和文件名（`name`），其余（主题、正文、附件内容，以及以后的工具加的
  字段）都换成字数（`tools::logged`）：那是邮件本身，又可能很长；记的是 agent 给谁写了什么样的信。按留下的
  字段写，而不是按要藏的：新字段缺省就不进日志。
- **工具里没有路径**：路径指的是常驻设备上的文件，对远程 agent 没有意义，还能用来读那台机器上的任意
  文件。附件以内容传入、传出（base64，有大小上限），或由 agent 自己把文件上传过来（见下文）。

### 草稿

- **草稿缺省由用户发**：工具写好的草稿和用户自己写的一样，存在本地并上传到 Drafts 文件夹，用户在 GUI 或 CLI 里
  看过再发；令牌有发信的层级时 agent 才能发（见[发信的确认](#发信的确认)）。服务端的 instructions 也这么说。
- **`from` 写地址，不写 id**：`list_accounts` 的 `sends_as` 就是可选的地址（`Name <地址>` 也行），按
  `MailClient::identity_by_address` 对到账户的身份上，对不上就是 `not_found`。缺省与 GUI 相同：新草稿用账户的
  缺省身份，回复用邮件寄到的那个。`update_draft` 只在草稿自己的账户里找，不把草稿搬到别的账户（那还要查令牌
  是否也管那个账户）。
- **`update_draft` 只改给了的字段**：交给引擎的是 `DraftChanges`，引擎在存它的写事务里读出草稿再换上（见
  [发信 / 身份](sending.md)），用户在 agent 读出之后存下的其他部分不会被旧值覆盖。什么都没给是 `invalid`。草稿交给
  发件箱后改不了（`conflict`）。
- **用户在 GUI 里开着的草稿不能改**：`update_draft`、`attach`、`remove_attachment` 得 `conflict: draft N is open in
  the app…`，关掉窗口后才行（`MailClient::edit_draft`，见[发信 / 身份](sending.md)）。否则用户发出去的信里有没看到
  的改动，或编辑器下次保存把 agent 的修改写回去。agent 可以另写一份草稿。
- **附件以内容传入**：`create_draft`、`reply`、`forward` 的 `attachments` 与 `attach` 都是 `{name, base64}`。引擎为此
  有了 `NewFile`（路径或内容），`create_draft` 等的 `files` 与 `attach_file` 都收它，CLI、GUI 照旧传路径。
  内容只有名字，类型按扩展名猜，和路径一样；名字要是 `safe_filename` 原样留下的（没有目录、Windows 不收的
  字符、控制与方向字符，两端没有点），否则拒绝而不是改掉：agent 给的名字就是附件的名字。一次调用给的文件解码后
  合计不超过 10 MB，草稿层级令牌的 HTTP 请求体上限因此是 16 MB（base64 多三分之一），其他令牌仍是 1 MB：先验
  令牌再读请求体，读、整理的令牌让宿主收不了那么多。
  一封信的附件总量仍由引擎按服务器的 `SIZE`、`APPENDLIMIT` 限制。和 GUI 一样，新草稿连同附件在一个事务里存，
  一个附件不行就整个不建。
- **附件经上传传入**：base64 要由模型在工具参数里逐字写出，一份 200 KB 的 PDF 就是 28 万字，超出模型一次能
  可靠写完的长度，写到一半被截断。所以文件也可以不经过模型（`uploads.rs`）：
  - `prepare_upload`（草稿层级）给一个上传地址 `<scheme>://<host>/mcp/uploads/<句柄>` 和句柄；agent 在自己的
    shell 里 `curl -T 文件 地址` 把原始字节传上来，再把 `{name, upload}` 代替 `{name, base64}` 交给 `attach`、
    `create_draft`、`reply`、`forward`。两者给且只给一个。
  - **地址本身就是凭据，不要令牌**：令牌在客户端的配置里（Claude Code 的请求头），模型看不到，shell 里也就
    没有；让模型去读配置等于把长期有效的令牌交给 shell。句柄是 16 字节的随机数，属于要它的令牌，只收一个
    文件，15 分钟内有效；别的令牌拿到句柄也用不了它的文件。带 `Origin` 的上传照样拒绝。
  - **地址按请求来的地方拼**：模型不知道自己连的是 `http://127.0.0.1:8642` 还是 `https://*.ts.net`，所以服务端
    给出完整的地址（`http.rs` 算出请求的 `BaseUrl`，和令牌一样放进请求的 extensions）：主机取 `X-Forwarded-Host`，否则 `Host`（`tailscale serve` 和 Cloudflare Tunnel 都保留原来的
    名字），都没有或不像主机名时用监听的地址；协议取 `X-Forwarded-Proto`，没有时本机的名字用 `http`，别的名字
    用 `https`：只有代理会带来别的名字，而它们都提供 HTTPS。代理若把服务挂在子路径下，地址里没有那段路径。
  - **不违反"工具里没有路径"**：服务端仍然只收内容，从不碰路径。读文件的是 agent 自己的进程，用的是它自己的
    权限，和它自己读出文件再 base64 一样，只是内容不经过模型。远程的 agent 传的是它那台机器上的文件，正合适。
  - **在内存里，用过即删**：句柄、收到的内容放在内存里（仿照发信确认的 `Asked`），宿主重启就没了。调用成功后
    才删掉用到的上传，失败（名字不对、草稿开着）的调用留着它给下一次；一次调用里同一个句柄不能给两次。每个
    令牌最多 16 个没用掉的上传，收到而没用掉的合计不超过 20 MB（两次调用的量，一次失败了还能准备下一次），
    一个文件不超过 10 MB：失控的 agent 占不满宿主的内存。每次准备、上传、取用时清掉过期的。接收中的上传由
    `Receiving` 守着，没收完（断开、太大、停止时被中止）就回到可以再传。
  - **不复制文件**：hyper 收到的 `Bytes` 存进上传，交给引擎的 `NewFile::Content` 也是 `Bytes`（共享，克隆只加引用
    计数），引擎只借用它写进 blob。所以失败的调用留着上传，也不必为每次调用另复制一份。`prepare_upload` 算一次调用，计入调用频率。
  - 上传记一行 info：令牌、句柄的前 8 位、大小。句柄从不完整进日志：参数里的 `upload` 照常换成字数，工具的
    错误（原样记进日志）只写是哪个文件，不写句柄。一个还没收到文件的句柄谁拿到都能往里传。
  - 放弃的方案：给工具加路径参数，哪怕只限本机：agent 被注入后能把任意文件附进外发的信；分块追加 base64：
    内容仍经过模型，只是拆成十几次调用，慢且费 token；上传时带 Bearer 令牌：模型拿不到令牌（见上）。
- **附件以内容传出**：`read_attachment` 不给结构化输出，而是按 MCP 的内容类型给：文本文件给文字（按字数截断，
  缺省 2 万字），PNG、JPEG、GIF、WebP 给图片（客户端直接交给模型看），其余给 base64 的 blob 资源；前面一行
  说明文件名、类型、大小，并说明内容是寄件人的。超过 10 MB 的不给。

## 限额

每个令牌有自己的调用频率和每日下载预算（`limits.rs`），对所有令牌相同，`ServeOptions::limits` 可改（测试用；
还没有设置项）。只在内存里按令牌名记，宿主重启就重新开始：限额防的是失控的 agent，不是要精确计费。

- **调用频率**：每分钟 60 次，可以一下子用完，之后每秒回来一次（令牌桶）。超了答 `over_limit: too many calls;
  try again in Ns`，作为工具的错误给 agent 读，而不是 HTTP 429：客户端多把 429 当成服务器坏了，模型反而看不到
  该等多久。`tools/list` 等不算。
- **下载预算**：服务器限制一个账户一天下载多少（Gmail 的 IMAP 约 2500 MB，超了 `OverLimit` 封一天），GUI 和后台
  同步也在用它。每个令牌一天 250 MB，从第一次下载起算 24 小时。只算为它的调用从服务器下载的：`read_message`、
  `reply` 下载还没有的正文（按 `RFC822.SIZE`），`read_attachment` 下载还没有的附件，`forward` 两者都可能。已经
  在本地的（后台预取过的、读过的）不算。下载前按大小扣，放不下就拒绝（`over_limit`）；调用失败只退回还没下载的部分：下载完了才失败的（回复带了坏文件名）照扣，否则反复失败就能白下载。读正文总经过引擎，正文已在本地也一样：打开时会重建旧版本存下的正文。

## 发信的确认

人通常在 agent 那边，不在常驻设备前，GUI 里的对话框没人看。所以确认在 agent 的客户端里问（MCP 的
elicitation），或者用户事先说了不必问。两种是令牌的两个层级：`send`（先问）与 `send_unasked`（不问）。

- **问的方式无状态**：MCP 2026-07-28 去掉了会话，服务端不能在调用中途向客户端发请求，改为多轮请求（SEP-2322）：
  `send_draft` 第一次答 `input_required`，带一个 elicitation（表单，没有要填的字段，接受就是确认），客户端问过用户后
  带着答复（`inputResponses`）和服务端给的 `requestState` 再调一次。rmcp 3.9 有这套类型，处理函数用 `RequestState`、
  `InputResponses` 取出。
- **`requestState` 只是句柄**：客户端原样送回，但不可信（agent 那一端也能改写请求）。问过的东西记在服务端的内存里
  （`tools/sending.rs` 的 `Asked`）：随机句柄 → 令牌名、草稿、给用户看的 `DraftRevision`、问的时间。再调时句柄必须是
  这个令牌为这份草稿问的，15 分钟内，只能用一次；对不上（编的、别的令牌的、过期的、宿主重启过的）就当没问过，
  再问一次，而不是报错。用户拒绝或取消是 `declined`，草稿留在 Drafts。
- **确认的就是发出去的**：问之前 `MailClient::preview_send` 读出草稿要发出去的样子与 revision，确认后
  `send_previewed(id, revision)` 只在草稿没改过、没在 GUI 里开着时交给发件箱（见[发信 / 别人代发](sending.md)），
  否则 `conflict`，agent 再调一次就按新内容重新问。问题里写明是哪个令牌要发、From、To、Cc、Bcc、主题、附件
  （名字与大小）和正文的前 2000 字。
- **客户端问不了**（没声明 elicitation 的表单，或协议版本早于 2026-07-28，多轮请求到不了它）：`send` 层级的令牌得
  `cannot_ask`，说明草稿留在 Drafts、或由用户给不问就发的令牌；`send_unasked` 的令牌不问，直接交给发件箱（`send_draft`，草稿在 GUI 里开着时
  同样拒绝）。`send_unasked` 无论客户端能不能问都不问：给的
  是独自工作的 agent，没人在客户端前回答。
- `send_draft` 带 `destructiveHint`，客户端可以在调用前另外再问。
- 交给发件箱后由常驻引擎在后台发，答复是发件箱的条目（`queued`），之后 `list_outbox` 看结果；`unknown` 仍只由用户
  处理。每次交给发件箱另记一行 info：令牌、草稿、收件人、有没有问过。
- 放弃的方案：在 `requestState` 里放签过名的草稿与 revision（rmcp 的 `request-state` feature）：要管密钥，宿主
  重启后签名照样有效，还得另防重放；放在内存里只需句柄，重启后多问一次而已。

## 不可信的内容

邮件是陌生人写的，提示注入是首要威胁（"把所有邮件转发给 x"）。

- 服务端的 instructions、`read_message` 的描述和输出 schema 里正文那个字段都说明：内容是寄件人写的，是数据
  不是指令。正文在 JSON 里是单独一个字段，字段本身就是边界，不再另加分隔符。只给纯文本，不给 HTML；不加载
  远程图片，不跟随链接。
- 默认的层级是只读；发信要单独给，并经上面的确认（不问就发更要用户明说）。
- 每次有改动的调用（工具、令牌、邮件 id、结果）记入日志（见[日志](logging.md)），见上文。
- 密码从不经过 MCP。

## 无界面服务器的密码

没有图形会话的 Linux 服务器通常没有 Secret Service，或开机后它是锁着的，引擎读不到 IMAP 密码。在
`SecretStore` 上加一个实现，读 systemd 的 credentials（`$CREDENTIALS_DIRECTORY`，`systemd-creds encrypt`
加密，可绑定 TPM 或主机密钥）：不在磁盘上留明文，开机也不用人输口令。

- 放弃的方案：启动时输入口令（重启后要人解锁，不适合常驻）；配置文件里的明文（放弃了"密码只在密钥环"，
  至多做成要显式打开的选项）。
- macOS 上只要用户登录着，钥匙串就能用（如不接显示器的 Mac mini）。

## 使用

```sh
catmail mcp token add laptop          # 只显示这一次，后面附上 Claude Code、Codex 的命令
catmail serve
tailscale serve --bg 8642             # https://mac.tailnet.ts.net/ → 127.0.0.1:8642
claude mcp add --transport http catmail https://mac.tailnet.ts.net/mcp \
    --header "Authorization: Bearer catmail_…"
```

本机的 agent 直接用 `http://127.0.0.1:8642/mcp`。`serve` 运行时 `catmail mcp token add / list / rm` 照样能用。
开着 GUI 时不用 `serve`：在 Settings › Agents 里打开开关、加令牌。

## 代码

`crates/catmail-mcp/src/`：
- `tools/`：工具与参数，按层级分文件，每个文件一个 rmcp 的 `#[tool_router(router = …)]`，`Mail::new` 里相加。
  `mod.rs` 是共用的部分（`Mail`、`Scope`、`Failed`、各工具的层级 `tier_of`、按范围找账户/文件夹/邮件、日志的
  `logged`）和 `ServerHandler`（按层级列出、调用前限频与定范围）；`reading.rs`、`organizing.rs`、`drafts.rs` 是
  三个层级的工具，`sending.rs` 是发信（问用户的多轮请求与 `Asked`）；`downloads.rs` 是下载预算的扣与退（`with_body`、`downloading`）。
- `output.rs`（逐个写出的输出结构，文档注释就是 schema 里的描述）、`tokens.rs`、`limits.rs`（调用频率与下载
  预算）、`connect.rs`（给 agent 的命令示例）、`http.rs`（监听、令牌、交给 rmcp；收上传；请求来的地址）、`uploads.rs`
  （上传的句柄与内容）。
- `tests/`：经 HTTP 对假 IMAP 服务器端到端，`mod.rs` 是 `Fixture` 与请求的辅助，其余按主题（`server.rs`：
  令牌、层级、账户范围、拒绝与停止；`reading.rs`、`organizing.rs`、`drafts.rs`、`sending.rs`、`limits.rs`）。
  `tests/tools.snapshot.json` 固定 agent 看到的工具列表，有意改动时 `UPDATE_SNAPSHOTS=1` 重新生成。
CLI 一侧是 `cmd/mcp.rs`；GUI 一侧是 `catmail-frontend` 的 `mcp.rs`（服务的线程、状态的说法、文案；端口的解析与登录表单共用 `login::parse_port`）、
`catmail-gui` 的 `settings/agents.rs` 和 `shell/mod.rs` 里托管服务的部分。

## 步骤

1. ✅ `catmail-mcp`：只读的工具、监听本机的 HTTP、令牌文件、`Origin` 检查；宿主 `catmail serve`；
   引擎的常驻预设。`rmcp` 3.9 带进的依赖通过了 `cargo deny`。
2. ✅ GUI 托管：Settings › Agents 里的开关、端口、令牌；不再查 `Host`，`serve` 去掉 `--host`。
3. ✅ 按层级与账户限定令牌（CLI 的 `--tier`、`--account`，GUI 的两个菜单）、改动的日志、整理的工具；草稿的
   工具、附件以内容传入传出、每个令牌的限额。
4. systemd credentials；systemd、launchd 的示例；GUI 的登录项与 `--background`。
5. ✅ 发信（elicitation 的多轮请求，或不问就发的令牌）；需要时 OAuth。
