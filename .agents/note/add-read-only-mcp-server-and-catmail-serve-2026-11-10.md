# 经 MCP 给 AI agent 只读访问：`catmail-mcp` 与 `catmail serve`

- 日期：2026-11-10
- 类型：功能

## 背景

想让 AI agent 经 MCP 读信、整理、写草稿：一台设备常驻，局域网和 tailnet 里的 agent 都能用，公网经 Tailscale
或 Cloudflare Tunnel。常驻设备可能是开着 GUI 的桌面，也可能是无界面的服务器。这次是第一步：只读。

## 决策

- **MCP 是库 `catmail-mcp`，由持有引擎的进程托管**：先做 `catmail serve`，以后加 GUI 的开关。放弃的方案：
  - `catmail mcp` 自己打开引擎：一个数据目录一个引擎，和常驻的 GUI 互相抢锁。
  - 先做通用 IPC：86 个门面方法、事件、流、大块数据、同步方法和错误都要过线，门面每改一次协议跟着改。
  - stdio 加 `gui.sock` 转发：只能在本机用。
- **只做 Streamable HTTP，只监听 127.0.0.1**，加密与可达性交给 `tailscale serve`：不做 TLS、不管证书。本机请求
  同样要令牌（隧道来的请求源地址也是本机）；`Host` 限定、带 `Origin` 的一律拒绝。不建会话，答复是 JSON。
- **令牌放在配置目录的单独文件**，只存 SHA-256：管令牌不用打开引擎，`serve` 运行时照样能增删，换宿主令牌不变；
  每个请求重读文件，删掉立即失效。
- **rmcp 3.9**，只开 server、macros、Streamable HTTP 服务端；底下直接用 hyper，不引入 axum。`cargo deny` 通过。
- 输出结构在 `output.rs` 里逐个写出，不复用 CLI 的 `json.rs`（字段面向 agent、要省 token，版本约定也不同）。
- 无界面服务器的密码改读 systemd credentials（`storage.md` 的待办），放弃启动时输入口令和明文配置文件；
  发信的确认走客户端（elicitation），不走 GUI 的对话框。这两项是后面的步骤。
- `MailClientBuilder::resident()` 收起 GUI 的引擎配置，`serve` 与 GUI 共用，两者行为不会漂移。`watch` 改为 `resident()` 去掉后台发信。
- 规则和名字放在引擎和类型里，CLI 与 MCP 共用：`MailClient::message_cursor`（翻页位置，只读邮件头）、
  `ErrorKind::as_str`、`Flags::names`；账户的 INBOX 未读数按 `special_folder`；core 转出 store 的
  `create_private_dir` / `private_file_options`，令牌文件与数据目录的权限规则是同一份。

## 方案

- 新 crate `catmail-mcp`：`tools.rs`（九个工具：账户、文件夹、邮件列表、搜索、读信、草稿、outbox、待同步的
  改动、同步）、`output.rs`、`tokens.rs`、`http.rs`、`tests.rs`、`tools.snapshot.json`。
- CLI：`catmail serve`（`--port`、`--host`，Ctrl-C 或 SIGTERM 停下）、`catmail mcp token add / list / rm`、全局
  `--config-dir`（`CATMAIL_CONFIG_DIR`）；日志过滤加上 `catmail_mcp`。GUI 的 `main.rs` 改用 `resident()`。
- 文档：新增 `docs/architecture/mcp.md`；`architecture.md`、`cli.md`、`storage.md`、README、AGENTS.md 相应更新。
- 验证：`catmail-mcp` 15 个测试，包括对假 IMAP 服务器的端到端 HTTP 测试（令牌、吊销、Host 与 Origin、工具列表
  快照、分页、搜索、读信后服务器上仍未读、同步报告（含服务器拒绝的改动）、停止时等下载答完、停止后不再监听）；CLI 加令牌命令与 JSON 快照测试。手动
  用 curl 对 `catmail serve` 走了一遍：无令牌 401、initialize、调用工具、运行中删令牌立即 401、SIGTERM 正常退出。
