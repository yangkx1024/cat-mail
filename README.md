# catmail

[![CI](https://github.com/yangkx1024/cat-mail/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/yangkx1024/cat-mail/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/yangkx1024/cat-mail)](https://github.com/yangkx1024/cat-mail/releases/latest)

用 Rust 编写的跨平台邮件客户端，同时提供图形界面（`catmail-gui`）和命令行（`catmail`）。

> 早期开发中：图形界面发布 macOS 版和 Linux 的 Flatpak，Windows 上只有命令行；接口和数据库结构
> 仍会变化。

## 特点

**轻量**

- 原生 Rust，界面是系统自己的控件（[Native Shell](https://github.com/longbridge/nsh)：Linux 上 GTK 4，macOS 上
  AppKit），邮件正文在系统的 WebView（WebKitGTK、WKWebView）里显示；没有 Electron，不自带浏览器内核
- 数据全在本地一个 SQLite 里，离线可读、可搜索、可操作；正文在后台按每日限额预取（没打开过的邮件也能按正文
  搜到，更早的搜索时问服务器），大附件打开时才下载
- 窗口关闭后在后台保持同步，同时释放界面占用的内存

**安全**

- 密码只存系统密钥环，不进数据库
- 邮件 HTML 先清洗再显示；正文页面不运行脚本，也不联网，远程图片默认拦截，要你点一下才由引擎下载
- 阅读不改变服务器上的任何东西（`EXAMINE`、`BODY.PEEK`）；打开即已读可以关闭
- 发出后没收到服务器确认的邮件不会自动重发，由你决定
- TLS 只用 rustls，禁止 OpenSSL 等第二套加密实现；全工作区禁用 `unsafe`；依赖每周做安全审计

**简洁高效**

- 三栏布局，多账户合并收件箱，写信用独立窗口；常用操作都有快捷键
- IMAP IDLE 推送新邮件，托盘（macOS 是菜单栏）显示未读数，Linux 上有系统通知
- 标记、移动、删除先改本地，再经队列同步到服务器，离线也能用
- 中日韩文字全文搜索；支持 Gmail 标签、一个账户多个发件身份
- 主题可直接导入终端配色（Alacritty、kitty、Ghostty、WezTerm、iTerm2 等）
- 命令行覆盖全部功能，`--json` 输出便于脚本调用

只支持 IMAP / SMTP 密码登录，不做 OAuth2：Gmail 等服务商可以用应用专用密码；只接受 OAuth2 的服务
（如 Microsoft 365、Outlook.com）不支持。

## 使用

### 安装

从 [GitHub Releases](https://github.com/yangkx1024/cat-mail/releases/latest) 下载。

Linux（x86_64、aarch64）：

- 命令行：`catmail-<版本>-<架构>-linux.tar.gz`（glibc 2.35 及以上，即 Ubuntu 22.04 之后的发行版）
- 图形界面：`catmail-<版本>-<架构>.flatpak`，`flatpak install --user catmail-<版本>-<架构>.flatpak` 安装（GNOME
  runtime 从 Flathub 自动装上），命令行也在包里：`flatpak run --command=catmail net.yangkx.catmail`

需要一个 Secret Service 提供者（如 gnome-keyring、KeePassXC）保存密码。

macOS（Apple 芯片，macOS 12 及以上，已签名并公证）：

- 图形界面：`catmail-<版本>-aarch64.dmg`，把 catmail 拖进"应用程序"
- 命令行：`catmail-<版本>-aarch64-macos.tar.gz`

密码保存在钥匙串里。

Windows：暂无安装包，目前只有命令行，可以从源码构建。

### 图形界面

```sh
catmail-gui
```

首次打开填写邮箱、密码和收发服务器，实际登录成功后才保存账户。之后的账户、身份、阅读、外观等都在
Settings 里。

常用快捷键：Ctrl N 新邮件、Ctrl R 回复、Ctrl Shift R 全部回复、Ctrl Shift F 转发、E 归档、Delete 删除、
Ctrl Shift U 已读 / 未读、Ctrl Shift L 旗标、Ctrl Shift M 移动、Ctrl F 搜索、F5 同步、Ctrl , 设置；
写信窗口里 Ctrl Enter 发送（macOS 上 Ctrl 换成 Cmd）。

### 命令行

```sh
# 账户（交互输入密码；脚本里用 --password-stdin）
catmail account add --email me@example.com --imap-host imap.example.com \
    --smtp-host smtp.example.com --smtp-security starttls
catmail account list --json

# 同步与阅读
catmail sync                    # 同步一次；catmail watch 持续同步直到 Ctrl-C
catmail ls INBOX -n 20          # 最新 20 封（标记列：* 未读，! 星标，@ 附件）
catmail read 42 --save ~/下载    # 阅读并保存附件
catmail search from:张三 季度报告

# 写信与发信
catmail compose --to li@example.com -s 周报 --body-file report.txt --attach 报表.xlsx --send
catmail reply 42 --all --body '收到，谢谢！'
catmail forward 42 --to li@example.com
catmail draft list              # 草稿；send <id> 发送
catmail outbox                  # 没发完的邮件；flush / retry / mark-sent / cancel

# 整理
catmail mark 42 43 --read --flag
catmail archive 42
catmail mv 42 --to Archive
catmail rm 42                   # 移到已删除；--permanent 永久删除
catmail folder add 2026 --parent Archive
```

`catmail <命令> --help` 查看全部选项；`--json` 的输出结构和退出码见
[docs/architecture/cli.md](docs/architecture/cli.md)。

同一个数据目录同时只能被一个进程打开：图形界面运行时，命令行不能使用同一个数据目录。

数据与配置目录：

| | 数据（`--data-dir` / `CATMAIL_DATA_DIR`） | 配置（`--config-dir` / `CATMAIL_CONFIG_DIR`） |
|---|---|---|
| Linux | `~/.local/share/catmail` | `~/.config/catmail` |
| macOS | `~/Library/Application Support/net.yangkx.catmail` | 同左 |
| Windows | `%APPDATA%\yangkx\catmail\data` | `%APPDATA%\yangkx\catmail\config` |

## 构建

工具链由 Nix flake 提供（flake 只能看到 git 跟踪的文件，新文件先 `git add -N`）：

```sh
nix develop
cargo build --release           # 产物：target/release/catmail、catmail-gui
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
```

不用 Nix 时，装好 `rust-toolchain.toml` 指定的 stable 工具链，Linux 上再装 GTK 4、WebKitGTK 6.0 和 GStreamer
的开发包（包名见 `.github/workflows/ci.yml`）。

对真实的 Dovecot 和 mailpit 测试（Linux，不能以 root 运行）：

```sh
nix develop .#testenv -c testenv/dovecot/run.sh   # 在另一个终端执行它打印的 export，然后：
cargo test -p catmail-core --test dovecot --test submission
```

参与开发（包括 AI agent）请先阅读 [AGENTS.md](AGENTS.md)。

## 架构

一个与 UI 无关的邮件引擎对外只暴露 `MailClient` 门面，命令行和图形界面是它之上平级的两个前端。

| crate | 职责 |
|---|---|
| `catmail-types` | 领域类型，纯数据 |
| `catmail-store` | SQLite、迁移、repository、内容寻址的 BlobStore |
| `catmail-proto` | IMAP / SMTP / MIME |
| `catmail-html` | 邮件 HTML 的清洗与读信区的处理（图片、深色配色），纯函数 |
| `catmail-core` | 引擎与 `MailClient` 门面，前端唯一依赖 |
| `catmail-cli` | `catmail` 命令行 |
| `catmail-frontend` | 桌面前端里与 UI 库无关的逻辑：设置、主题、侧栏与列表的模型、命令、写信与发件箱的模型、托盘等 |
| `catmail-gui` | `catmail-gui` 图形界面（Native Shell：GTK 4 / AppKit，正文在 WebView 里） |
| `catmail-testkit` | 可注入故障的假 IMAP / SMTP 服务器，只用于测试 |

依赖方向：`types ← store, proto ← core ← cli / gui`，`core ← frontend ← gui`。设计细节见
[docs/architecture.md](docs/architecture.md) 及 [docs/architecture/](docs/architecture/)。

## 路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | 存储、密钥环、数据目录锁、`MailClient` 门面 | ✅ |
| M1 | IMAP 只读同步、协议测试环境 | ✅ |
| M2 | 正文与附件按需下载、全文搜索、blob GC | ✅ |
| M3 | 草稿、SMTP 发信与 outbox、保存到已发送 | ✅ |
| M4 | 离线操作队列、IDLE 推送与后台同步、草稿同步到服务器 | ✅ |
| M5 | 图形界面（起初基于 GPUI，后改用 Native Shell） | ✅ |
| M6 | 打包：macOS 磁盘映像与 CLI ✅；Linux 图形界面的 Flatpak ✅（上 Flathub 待做）；Windows（图形界面接上 Native Shell 的 Windows 后端） | 进行中 |

之后：前端通过 IPC 连接正在运行的引擎（让图形界面和命令行能同时使用）。

## 许可

双许可，任选其一：[Apache License 2.0](LICENSE-APACHE) 或 [MIT License](LICENSE-MIT)。

除非你明确声明，否则你有意提交给本项目的任何贡献，按 Apache-2.0 许可的定义，都将按上述双许可授权，
不附加任何其他条款或条件。
