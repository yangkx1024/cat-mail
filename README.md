# catmail

[![CI](https://github.com/yangkx1024/cat-mail/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/yangkx1024/cat-mail/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/yangkx1024/cat-mail)](https://github.com/yangkx1024/cat-mail/releases/latest)

用 Rust 编写的跨平台邮件客户端，同时提供图形界面（`catmail-gui`）和命令行（`catmail`）。

> 早期开发中：图形界面已发布 macOS 磁盘映像与 Linux Flatpak，Windows 目前仅提供命令行；接口与数据库结构仍可能变化。

## 特点

### 轻量

- **原生界面**：基于 [Native Shell](https://github.com/longbridge/nsh)（Linux 用 GTK 4，macOS 用 AppKit），正文交给系统 WebView（WebKitGTK / WKWebView）；没有 Electron，不自带浏览器内核。
- **本地优先**：全部数据存在单个本地 SQLite 中，离线可读、可搜索、可操作；正文在后台按每日限额预取（没打开过的邮件也能搜到正文，更早的搜索时自动问服务器），大附件打开时才下载。
- **低开销常驻**：关闭窗口后可在后台继续同步，同时释放窗口与 WebView 渲染进程占用的内存。

### 安全

- **凭据隔离**：密码只存系统密钥环，不进 SQLite。
- **不可信内容隔离**：邮件 HTML 先清洗再显示；正文页面禁用脚本且断网（CSP 拦截一切外链请求），远程图片默认不加载，点击后才由引擎代为下载（不带 cookie 和 Referer）。
- **只读不改状态**：同步和阅读只用 `EXAMINE` 与 `BODY.PEEK`，不修改服务器状态；“打开即已读”可在设置中关闭。
- **发信绝不偷偷重发**：发出后未收到服务器确认的邮件（`unknown`）不会自动重发，交由用户决定。
- **最小信任面**：TLS 仅用 `rustls`（禁止 OpenSSL 等第二套加密栈）；除桌面前端调用 AppKit、`malloc_trim` 等少量系统接口外，所有 crate 禁用 `unsafe`；依赖每周自动做 `cargo deny` 审计。

### 简洁高效

- **专注阅读与写作**：三栏布局、多账户合并收件箱（All Inboxes）、独立富文本写信窗口（支持格式与内嵌图片），常用操作均有快捷键。
- **实时提醒**：IMAP IDLE 推送新邮件，托盘（macOS 为菜单栏与 Dock）显示未读数，支持系统通知与开机自启。
- **离线队列**：标记、移动、删除先改本地再经队列同步到服务器，离线照常可用。
- **检索与组织**：中日韩文字全文搜索、视图按标记过滤；完整支持 Gmail 标签与单账户多发件身份。
- **终端主题导入**：可直接导入 Alacritty、kitty、Ghostty、WezTerm、iTerm2 等终端配色。
- **命令行与 AI Agent**：命令行覆盖全部功能，`--json` 输出便于脚本调用；内置 MCP 服务（`catmail serve` 或在图形界面托管），可按权限层级和账户范围让 AI agent 读信、整理、写草稿或经确认后发信。

> **登录范围**：只支持 IMAP / SMTP 密码登录，不做 OAuth2。Gmail 等服务商可使用应用专用密码；仅允许 OAuth2 的服务（如 Microsoft 365、Outlook.com）不支持。

## 使用

### 安装

从 [GitHub Releases](https://github.com/yangkx1024/cat-mail/releases/latest) 下载：

- **Linux（x86_64、aarch64）**
  - 图形界面：运行 `flatpak install --user https://catmail.yangkx.net/flatpak/net.yangkx.catmail.flatpakref` 安装，之后随 `flatpak update` 和软件中心自动更新，应用里也会提示新版本、一键更新并重启（缺的 GNOME runtime 会从 Flathub 自动安装）。也可以下载 `catmail-<版本>-<架构>.flatpak`，用 `flatpak install --user` 安装，同样会自动更新。命令行一并打在包里：`flatpak run --command=catmail net.yangkx.catmail`。
  - 命令行：`catmail-<版本>-<架构>-linux.tar.gz`（需 glibc 2.35 及以上，即 Ubuntu 22.04 及之后的发行版）。
  - 密码存储需要 Secret Service 提供者（如 `gnome-keyring`、KeePassXC）。
- **macOS（Apple 芯片，macOS 12 及以上，已签名并公证）**
  - 图形界面：`catmail-<版本>-aarch64.dmg`，将 `catmail.app` 拖进“应用程序”（开机启动需 macOS 13 及以上）。
  - 命令行：`catmail-<版本>-aarch64-macos.tar.gz`。
  - 密码保存在系统钥匙串中。
- **Windows**
  - 暂无安装包，目前仅提供命令行，可从源码构建。

### 图形界面

```sh
catmail-gui
```

首次打开时填写邮箱、密码和收发服务器，实际登录（IMAP 与 SMTP）成功后才保存账户。之后的账户、发件身份、阅读、外观与 MCP 设置都在 **Settings** 中。

常用快捷键（macOS 上将 `Ctrl` 换为 `Cmd`）：

| 区域 | 操作 | 快捷键 |
|---|---|---|
| 主窗口 | 新建邮件 / 回复 / 全部回复 / 转发 | `Ctrl+N` / `Ctrl+R` / `Ctrl+Shift+R` / `Ctrl+Shift+F` |
| 主窗口 | 归档 / 删除 / 移动 / 标签（Gmail） | `E` / `Delete` / `Ctrl+Shift+M` / `L` |
| 主窗口 | 已读未读 / 旗标 / 勾选多封 / 重命名文件夹 | `Ctrl+Shift+U` / `Ctrl+Shift+L` / `X` / `F2` |
| 主窗口 | 搜索 / 立即同步 / 关读信区 / 设置 | `Ctrl+F` / `F5`（或 `Ctrl+Shift+N`） / `Esc` / `Ctrl+,` |
| 全局 | 放大 / 缩小 / 重置字号 | `Ctrl++` / `Ctrl+-` / `Ctrl+0` |
| 写信窗口 | 发送 / 添加附件 / 插入链接 / 关闭 | `Ctrl+Enter` / `Ctrl+Shift+A` / `Ctrl+K` / `Esc` |

### 命令行

```sh
# 账户（交互输入密码；脚本里用 --password-stdin）
catmail account add --email me@example.com --imap-host imap.example.com \
    --smtp-host smtp.example.com --smtp-security starttls
catmail account list --json

# 同步与阅读
catmail sync                    # 同步一次；catmail watch 持续同步直到 Ctrl-C
catmail ls INBOX -n 20          # 最新 20 封（标记列：* 未读，! 星标，@ 附件）
catmail ls INBOX --only unread  # 按标记过滤：unread / read / flagged / unflagged（search 也支持）
catmail read 42 --save ~/下载    # 阅读并保存附件
catmail search from:张三 季度报告

# 写信与发信
catmail compose --to li@example.com -s 周报 --body-file report.txt --attach 报表.xlsx --send
catmail reply 42 --all --body '收到，谢谢！'
catmail forward 42 --to li@example.com
catmail draft list              # 草稿列表；catmail send <id> 发送
catmail outbox                  # 未发完的邮件；flush / retry / mark-sent / cancel

# 整理
catmail mark 42 43 --read --flag
catmail archive 42
catmail mv 42 --to Archive
catmail rm 42                   # 移到已删除；--permanent 永久删除
catmail label 42 --add Work     # Gmail 标签
catmail folder add 2026 --parent Archive

# MCP：让 AI agent 经 127.0.0.1:8642/mcp 使用邮箱（开着图形界面时改在 Settings › Agents 中开启）
catmail mcp token add laptop    # 生成令牌（只显示一次）；--tier read|organize|draft|send|send_unasked，--account 限定账户
catmail serve                   # 常驻同步并提供 MCP；其他设备可经 tailscale serve 访问，见 docs/architecture/mcp.md
```

运行 `catmail <命令> --help` 查看全部选项；`--json` 结构与退出码说明见 [docs/architecture/cli.md](docs/architecture/cli.md)。

> 同一个数据目录同时只能被一个进程打开：图形界面（含后台常驻时）运行期间，命令行不能访问同一个数据目录；MCP 令牌文件除外，`catmail mcp token` 随时可用。

### 数据与配置目录

| 平台 | 数据目录（`--data-dir` / `CATMAIL_DATA_DIR`） | 配置目录（`--config-dir` / `CATMAIL_CONFIG_DIR`） |
|---|---|---|
| Linux | `~/.local/share/catmail`（Flatpak：`~/.var/app/net.yangkx.catmail/data/catmail`） | `~/.config/catmail` |
| macOS | `~/Library/Application Support/net.yangkx.catmail` | 同左 |
| Windows | `%APPDATA%\yangkx\catmail\data` | `%APPDATA%\yangkx\catmail\config` |

## 构建

- **Linux（推荐 Nix flake）**：flake 只可见 git 跟踪的文件，新文件需先 `git add -N`：

```sh
nix develop
cargo build --release           # 产物：target/release/catmail、target/release/catmail-gui
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
```

不用 Nix 时，安装 `rust-toolchain.toml` 指定的 `stable` 工具链，以及 GTK 4 与 WebKitGTK 6.0（2.42 及以上）开发包（包名见 [`.github/workflows/ci.yml`](.github/workflows/ci.yml)）。

- **macOS**：flake 不提供 macOS dev shell；安装 `rust-toolchain.toml` 指定的工具链和 `cargo-deny` 后，直接运行上面的 `cargo` 命令即可（打包 `.dmg` 另需 Xcode）。

对真实的 Dovecot 和 mailpit 运行协议测试（Linux，不能以 root 运行）：

```sh
nix develop .#testenv -c testenv/dovecot/run.sh   # 在另一个终端执行它打印的 export，然后：
cargo test -p catmail-core --test dovecot --test submission
```

参与开发（包括 AI agent）请先阅读 [AGENTS.md](AGENTS.md)。

## 架构

核心是一个与 UI 无关的邮件引擎，对外只暴露 `MailClient` 门面；命令行与图形界面是其上平级的两个前端。

| crate | 职责 |
|---|---|
| `catmail-types` | 领域类型，纯数据 |
| `catmail-store` | SQLite、迁移、repository、FTS5、内容寻址 `BlobStore` |
| `catmail-proto` | IMAP / SMTP / MIME、远程图片 HTTP 下载 |
| `catmail-html` | 邮件 HTML 清洗、读信区图片/深色改写与转纯文本，纯函数 |
| `catmail-core` | 引擎与 `MailClient` 门面，前端唯一依赖 |
| `catmail-mcp` | 经 MCP 提供给 AI agent 的工具、令牌与本机 HTTP 服务 |
| `catmail-frontend` | 桌面端与 UI 库无关的逻辑：设置、主题、侧栏/列表/写信/发件箱模型、命令、托盘、开机启动、MCP 托管 |
| `catmail-cli` | `catmail` 命令行 |
| `catmail-gui` | `catmail-gui` 图形界面（Native Shell：GTK 4 / AppKit，正文在 WebView 中） |
| `catmail-testkit` | 可注入故障的假 IMAP / SMTP 服务器（仅测试依赖） |

依赖方向：`types ← store, proto ← core ← cli / gui`，`html ← proto, core`，`core ← frontend ← gui`，`core ← mcp ← cli, frontend`。设计细节见 [docs/architecture.md](docs/architecture.md) 及 [docs/architecture/](docs/architecture/)。

## 路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | 存储、密钥环、数据目录锁、`MailClient` 门面 | ✅ |
| M1 | IMAP 只读同步、协议测试环境 | ✅ |
| M2 | 正文与附件按需下载、全文搜索、blob GC | ✅ |
| M3 | 草稿、SMTP 发信与 outbox、保存到已发送 | ✅ |
| M4 | 离线操作队列、IDLE 推送与后台同步、草稿同步到服务器 | ✅ |
| M5 | 图形界面（起初基于 GPUI，后迁至 Native Shell） | ✅ |
| M6 | 打包：macOS `.dmg` 与 CLI ✅；Linux Flatpak（自己的仓库，自动更新）与 CLI ✅；Windows 图形界面（接入 Native Shell Windows 后端）与打包 | 进行中 |
| M7 | MCP（[设计](docs/architecture/mcp.md)）：只读、整理、草稿与附件、令牌层级/账户范围/限额、经确认发信、`catmail serve` 与 GUI 托管 ✅；无界面服务器密码（systemd credentials）与 systemd / launchd 服务示例 | 进行中 |

之后：前端通过 IPC 连接正在运行的引擎（让图形界面与命令行可同时访问同一数据目录）。

## 许可

双许可，任选其一：[Apache License 2.0](LICENSE-APACHE) 或 [MIT License](LICENSE-MIT)。

除非你明确声明，否则你有意提交给本项目的任何贡献，按 Apache-2.0 许可的定义，都将按上述双许可授权，不附加任何其他条款或条件。
