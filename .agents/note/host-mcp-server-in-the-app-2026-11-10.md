# 由图形界面托管 MCP 服务：Settings › Agents

- 日期：2026-11-10
- 类型：功能

## 背景

MCP 的第 2 步（`docs/architecture/mcp.md`）。第 1 步只有 `catmail serve` 能托管 MCP 服务，可常驻的桌面上开着 GUI
时它占着数据目录，`serve` 打不开引擎：桌面用户没法让 agent 连上来。

## 决策

- **服务的线程放在 `catmail-frontend`（`mcp::Host`），不放 `catmail-gui`**：启停、换端口、只看最新的设置、失败的
  说法都不需要 UI 库，放在 frontend 里能起真实的服务来测。依赖方向因此是 `core ← mcp ← cli, frontend ← gui`；GUI
  经 frontend 重新导出的 `Tokens` 管令牌，不直接依赖 `catmail-mcp`。
- **一个线程、一个 current-thread runtime，跟应用一样长**：服务开关多次也不新建线程；设置经无界通道交过去，线程
  只取最新的一个。放弃的方案：在 `catmail-mcp` 里加"在自己的线程上启动"的 API，每开一次一个线程，启停的先后
  （旧的没停完新的就绑端口）要由调用方排。
- **外壳交出的是当时生效的设置**，不是这次修改的：两个视图线程上的修改可能以任意顺序到达，晚到的旧值会把服务
  改回去。外壳像视图一样跟随设置（`follow_settings`），`update_settings` 不为 MCP 开特例；跟随设置和状态的
  两份广播收成 `catmail_frontend::watched::Watched<T>`：读当前值与开始跟随、改值与通知跟随者都在同一把锁下。
- **顺手修掉设置的两个竞争**（原有的问题）：视图先读设置再跟随，两步之间别的窗口改的设置会漏掉，视图停在旧值；
  `update_settings` 在释放锁之后才排队写文件、通知跟随者，两个线程同时改时可能倒序到达，跟随者停在较旧的值，
  文件也可能写成较旧的。设置改用 `Watched`，写文件的排队放进它的锁里。
- **同样的设置不重试**：改文字大小等别的设置也会走到这里，若失败后每次都重绑端口，状态会来回闪；关掉再打开才
  重试。
- **状态只有三种**：没在监听、在监听、失败。"没在监听"在端口释放后才报告；开关开着而没在监听就显示
  "Starting…"（`Status::line`），不另设"启动中"。GUI 停服务只等 5 秒（`serve` 30 秒）；退出时先停服务
  再关引擎。
- **端口在 Return 或失焦时生效**，逐字生效会把服务在中间的端口上依次开一遍。
- **新令牌就地显示**在令牌列表下面（等宽、可选中、复制按钮），不再开一层对话框：设置本身已是对话框。加令牌
  用表单共用的 `Submission` / `form::submit` / `form::error`（`Submission::ended` 改为接受任何可显示的错误），
  删除用 `form::confirm_then`（从 Accounts 移来）。
- **不再查 `Host`，`serve` 的 `--host` 去掉**，GUI 里也就没有"隧道的主机名"这一项。`tailscale serve` 转来的请求
  `Host` 是 `*.ts.net`，查它就得让每个用隧道的人把名字再配一遍，漏配时被拒且报错不指向这项配置。而它防的 DNS
  rebinding 已经被令牌（浏览器不会自动带 `Authorization`）和拒绝带 `Origin` 的请求（浏览器的 POST 总带它）挡住，
  剩下的只是看到 401。
- **dev 构建里依赖用 `opt-level = 1`**（根 `Cargo.toml`，本仓库的 crate 仍是 0）：GUI 链接进 rmcp、hyper 后，
  debug 版 `catmail-gui` 的 `__eh_frame` 从 15.4 MB 涨到 16.8 MB，超过 macOS 链接器能编码的 16 MB，每次链接都警告。
  依赖优化后降到 8.8 MB，debug 版也跑得快些；代价是依赖要整体重编一次。放弃的方案：`allow(linker_messages)`
  会把以后真正的链接器消息一起藏掉。
- **怎么接上 agent 就写在界面上**：Settings › Agents 末尾"Connect an agent"给出 Claude Code、Codex 的命令，刚加的
  令牌直接填进去；`catmail mcp token add` 也在 stderr 打出同样的命令（stdout 仍只有令牌，方便脚本取用）。文字在
  `catmail_mcp::connect` 里，两边共用。Codex 只能从环境变量读令牌（`bearer_token` 在它的配置里不被支持），示例
  因此是 `codex mcp add … --bearer-token-env-var CATMAIL_MCP_TOKEN` 加一行 `export`。长命令在选项之间用 `\` 换行，
  界面里窄的时候不会从 `--header` 中间折开。
- **review 后修的两处视图生命周期问题**（codex 指出）：
  - 设置对话框的 `dynamic` 在建一节时把节里读到的设置也订阅了（Agents 的端口框读了一次），之后开关或端口一变
    整节重建，刚显示的令牌和正在输入的名字随之丢掉。改为只按选中的节重建，建节的过程 `untracked`（nsh 的
    `dynamic` 用法）。其他几节读设置都在绑定里，不受影响。
  - 令牌的读取、新增、删除在 `in_view`（属于邮箱）里跑，离开这一节或关掉设置后才结束时，写已销毁的信号会
    panic；令牌文件被锁住时等得更久。读取改为属于这一节（`effect` 里 `spawn`，随节结束取消）；新增、删除照常
    做完，只在这一节还在时更新显示（`form::submit` 在表单已不在时不再写它的信号）。Settings › Reading 量、清
    图片缓存原有同样的问题，一并改了。
- **Windows 上工具列表快照的测试一直失败**（#275 合并后 `main` 就是红的）：GitHub 的 Windows runner 检出文本文件时
  换成 CRLF，`include_str!` 嵌进去的快照带 `\r\n`，生成的 JSON 是 `\n`。`.gitattributes` 把
  `tools.snapshot.json` 定为 `eol=lf`，和 `.eml` 夹具一样按字节保持。
- 新增 heroicons：`sparkles`（导航）、`clipboard-document`（复制），`key` 原已在资源里。

## 方案

- `catmail-mcp`：`ServeOptions` 可比较、去掉 `hosts`，rmcp 改用 `disable_allowed_hosts`，导出 `PATH`；新增
  `connect.rs`（`local_url`、`examples`）。CLI 去掉 `serve --host`，`mcp token add` 附上命令示例。
- `catmail-frontend`：`settings::McpSettings`（`enabled`、`port`，缺省关、8642）；`mcp.rs`：`Host`
  （`start` / `want` / `shut_down`）、`Status`、`serve_options`、`endpoint`、`parse_port`、`remove_token` 的
  确认文案；`SettingsFile::dir`。
- `catmail-gui`：`shell/mod.rs` 在引擎打开后起 `Host`，设置的 MCP 部分变了就 `want`，状态转给跟随的视图，退出时
  先 `shut_down`；`settings/agents.rs` 新的一节（开关、端口、令牌、连接示例）；日志过滤加上 `catmail_mcp`。
- 文档：`mcp.md` 新增"GUI 托管"、连接示例和不查 `Host` 的理由、第 2 步打勾；`cli.md` 去掉 `--host`、写上
  `add` 打出的示例；`gui.md`、`architecture.md`、README、AGENTS.md、ROADMAP 相应更新。
- 验证：frontend 新增 5 个测试（跟随设置启停与换端口、同样的设置不重启、端口被占时的说法及关开后重试、关闭时
  停下、选项、状态的说法；端口的解析与登录表单共用 `login::parse_port`）；`catmail-mcp` 的测试改为：任意 `Host` 带令牌可用，DNS rebinding 的请求没有令牌得 401、
  带 `Origin` 得 403；设置文件的读写测试覆盖新字段。手动：临时数据目录启动 GUI，开关打开时 curl 无令牌 401、有
  令牌 initialize 200、`Host: mac.tailnet.ts.net` 不用配置也是 200、带 `Origin` 403；端口被占时显示
  "Port 8643 is in use by another program."；SIGTERM 退出时端口随之关闭。示例命令：把 `token add` 打出的两段经
  `sh` 在临时的 `CLAUDE_CONFIG_DIR`、`CODEX_HOME` 里执行，配置都写对了，`claude mcp list` 对 `catmail serve` 报
  Connected；Codex 没有真正连上过（要登录的 Codex，`codex mcp list` 不连接服务器）。`catmail-mcp`、CLI 各加一个
  测试固定示例的文字。`Watched` 的三个测试里，8 个线程各改 500 次、检查跟随者和"写文件"都按顺序收到全部 4000 个
  值：把通知挪回锁外（原来的写法）时 3 次都失败，修好后 3 次都通过。新增令牌、复制、删除的点击流程没有手动
  走过。
