# cat-mail

用 Rust 编写的跨平台邮件客户端。核心是一个与 UI 无关的邮件引擎（账户、收件箱、草稿、发件、
附件），对外只暴露 `MailClient` 门面；在它之上有两个平级的前端：

- **`catmail`**：命令行，可直接在终端使用，也支持 `--json` 供脚本调用
- **图形界面**：基于 [GPUI](https://www.gpui.rs/) 与 [gpui-kit](https://github.com/longbridge/gpui-kit)（规划中）

数据存储在本地 SQLite，离线可用；密码保存在系统密钥环中，不进数据库。

> 项目处于早期开发阶段（M0 已完成），接口和数据库结构仍会变化。

架构设计见 [docs/architecture.md](docs/architecture.md)。

## 范围

- **平台**：先做 Linux，之后支持 Windows 与 macOS。核心层不包含平台相关代码。
- **第一阶段**：通用 IMAP / SMTP + 密码认证。暂不支持 Gmail 与 Microsoft 365（两者需要
  OAuth2），认证做成可替换的接口，留到 M6。

## 规划

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | workspace；types；store（迁移、事务接口）；BlobStore；keyring；数据目录锁；`MailClient` 门面；`catmail account` | ✅ 完成 |
| M1 | IMAP 只读同步（文件夹、邮件头）；认证接口；协议测试环境；`catmail sync` / `catmail ls` | |
| M2 | 正文与附件按需下载；中文分词 + 全文搜索；blob GC；最小读信窗口；`catmail read` / `catmail search` | |
| M3 | 草稿；SMTP 发信与 outbox 状态机；保存到 Sent | |
| M4 | 离线操作队列（标记 / 移动 / 删除）；IDLE 推送 | |
| M5 | 图形界面：三栏布局、读信、写信 | |
| M6 | OAuth2（Gmail / Microsoft 365）；Windows / macOS 打包 | |

几个安排上的考虑：

- **最小读信窗口放在 M2。** gpui-kit 的 HTML 渲染定位为简单 HTML，而真实邮件大量使用表格
  布局和内联样式。一拿到正文就用真实邮件验证表格、`cid:` 内嵌图片和中文排版，不等到 M5。
- **协议测试环境从 M1 开始建。** 用 docker 跑 Stalwart 或 Dovecot，再加故障注入，覆盖断线、
  UIDVALIDITY 改变、重复或乱序响应、服务器缺少 CONDSTORE/QRESYNC、进程在同步中途重启。
  M3 加入 SMTP 确认丢失和保存到 Sent 失败的场景。
- **IPC 待定。** 同一个数据目录同时只允许一个引擎（见架构文档），所以目前 GUI 运行时 CLI
  不能打开同一个数据目录。之后让其他前端通过 IPC 连接正在运行的引擎。

## 使用

目前只能管理账户，还不会连接服务器：

```sh
# 添加账户（交互输入密码；脚本里用 --password-stdin）
catmail account add --email me@example.com --name "Me" \
    --imap-host imap.example.com --smtp-host smtp.example.com --smtp-security starttls

catmail account list            # 加 --json 输出 JSON
catmail account remove me@example.com
```

端口默认按加密方式推断：IMAP `tls` 为 993、`starttls` / `plain` 为 143；SMTP `tls` 为 465、
`starttls` 为 587、`plain` 为 25。

数据目录默认为 Linux `~/.local/share/catmail`、macOS
`~/Library/Application Support/dev.CatMail.CatMail`、Windows `%APPDATA%\CatMail\CatMail\data`，
可用 `--data-dir` 或环境变量 `CATMAIL_DATA_DIR` 覆盖。

Linux 上需要一个 Secret Service 提供者（如 gnome-keyring、KeePassXC）来保存密码。

## 开发

工具链由 Nix flake 提供：

```sh
nix develop                                         # 进入开发环境（Rust stable、clippy、rustfmt、sqlite）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p catmail-cli -- account list
```

flake 只能看到 git 跟踪的文件，新加的文件需要先 `git add -N`。不用 Nix 时，装好
`rust-toolchain.toml` 指定的 stable 工具链即可。

CI（GitHub Actions）：

- `ci.yml`：rustfmt 检查；在 Linux、macOS、Windows 上分别跑 clippy（`-D warnings`）和全部
  测试。目前只发布 Linux 版，但核心层必须保持跨平台，所以从一开始就在三个平台上测试。
  只改文档（`*.md`、`docs/`、`.agents/`、许可证）时不触发。
- `nix.yml`：`flake.nix` / `flake.lock` 变更时检查 Nix 格式、`nix flake check`，并确认开发
  环境可用。

| crate | 职责 |
|---|---|
| `catmail-types` | 领域类型，纯数据 |
| `catmail-store` | SQLite、迁移、repository、BlobStore |
| `catmail-proto` | IMAP / SMTP / MIME 适配 |
| `catmail-core` | 引擎与 `MailClient` 门面，前端唯一依赖 |
| `catmail-cli` | `catmail` 命令行 |

参与开发（包括 AI agent）请先阅读 [AGENTS.md](AGENTS.md)。

## 许可

本项目采用双许可，你可以任选其一：

- Apache License 2.0（[LICENSE-APACHE](LICENSE-APACHE)）
- MIT License（[LICENSE-MIT](LICENSE-MIT)）

除非你明确声明，否则你有意提交给本项目的任何贡献，按 Apache-2.0 许可的定义，都将按上述
双许可授权，不附加任何其他条款或条件。
