# cat-mail

用 Rust 编写的跨平台邮件客户端。核心是一个与 UI 无关的邮件引擎（账户、收件箱、草稿、发件、
附件），对外只暴露 `MailClient` 门面；在它之上有两个平级的前端：

- **`catmail`**：命令行，可直接在终端使用，也支持 `--json` 供脚本调用
- **`catmail-gui`**：图形界面，基于 [GPUI](https://www.gpui.rs/) 与 [gpui-kit](https://github.com/longbridge/gpui-kit)（目前只有最小读信窗口）

数据存储在本地 SQLite，离线可用；密码保存在系统密钥环中，不进数据库。

> 项目处于早期开发阶段（M3 已完成），接口和数据库结构仍会变化。

架构设计见 [docs/architecture.md](docs/architecture.md)。

## 范围

- **平台**：先做 Linux，之后支持 Windows 与 macOS。核心层不包含平台相关代码。
- **第一阶段**：通用 IMAP / SMTP + 密码认证。暂不支持 Gmail 与 Microsoft 365（两者需要
  OAuth2），认证做成可替换的接口，留到 M6。

## 规划

| 阶段 | 内容 | 状态 |
|---|---|---|
| M0 | workspace；types；store（迁移、事务接口）；BlobStore；keyring；数据目录锁；`MailClient` 门面；`catmail account` | ✅ 完成 |
| M1 | IMAP 只读同步（文件夹、邮件头）；认证接口；协议测试环境；`catmail sync` / `catmail ls` | ✅ 完成 |
| M2 | 正文与附件按需下载；中文分词 + 全文搜索；blob GC；最小读信窗口；`catmail read` / `catmail search` | ✅ 完成 |
| M3 | 草稿；SMTP 发信与 outbox 状态机；保存到 Sent；`catmail compose` / `reply` / `send` / `outbox` | ✅ 完成 |
| M4 | 离线操作队列（标记 / 移动 / 删除）；IDLE 推送 | |
| M5 | 图形界面：三栏布局、读信、写信 | |
| M6 | OAuth2（Gmail / Microsoft 365）；Windows / macOS 打包 | |

几个安排上的考虑：

- **最小读信窗口放在 M2。** gpui-kit 的 HTML 渲染定位为简单 HTML，而真实邮件大量使用表格
  布局和内联样式。一拿到正文就用真实邮件验证表格、`cid:` 内嵌图片和中文排版，不等到 M5。
  结果：段落、数据表格、内嵌图片和中文排版正常；用表格做布局的邮件（多数营销邮件）原本会被
  压成一段，现在渲染前先把布局表格展开成块（见架构文档"GUI"一节），"在浏览器打开"作为兜底。
- **协议测试环境从 M1 开始建。** 一个可注入故障的进程内假 IMAP 服务器（`catmail-testkit`，
  三个平台的 CI 都跑），覆盖断线、UIDVALIDITY 改变、重复或乱序响应、服务器缺少 CONDSTORE、
  STARTTLS 明文注入、同步中途中断后续传；再加一个真实的 Dovecot（Nix 提供，不需要 docker）。
  M3 加入了假 SMTP 服务器，覆盖 SMTP 确认丢失、连接中断、收件人被拒、保存到 Sent 失败和
  APPEND 答复丢失等场景；发信也对真实服务器测试：Dovecot 的 submission 服务（STARTTLS、
  隐式 TLS、AUTH、SIZE），转发给 mailpit 核对收到的内容。
- **草稿暂时只在本地。** 把草稿同步到服务器的"草稿箱"需要每次保存都删除旧副本，留到 M4 的
  离线操作队列。
- **IPC 待定。** 同一个数据目录同时只允许一个引擎（见架构文档），所以目前 GUI 运行时 CLI
  不能打开同一个数据目录。之后让其他前端通过 IPC 连接正在运行的引擎。

## 使用

目前可以管理账户、同步文件夹和邮件头、列出和阅读邮件、搜索（只读，不改服务器上的状态），
以及写信、回复和发信：

```sh
# 添加账户（交互输入密码；脚本里用 --password-stdin）
catmail account add --email me@example.com --name "Me" \
    --imap-host imap.example.com --smtp-host smtp.example.com --smtp-security starttls

catmail account list            # 加 --json 输出 JSON
catmail account remove me@example.com

catmail sync                    # 同步所有账户；也可以指定 id 或邮箱
catmail ls                      # 列出文件夹（多个账户时用 -a 选择）
catmail ls INBOX -n 20          # 最新的 20 封邮件；文件夹名可以是中文，如 `catmail ls 已发送`

catmail read 42                 # 阅读 ID 为 42 的邮件（第一次会下载正文）
catmail read 42 --html          # 清洗后的 HTML；--raw 输出原文（.eml）
catmail read 42 --save ~/下载    # 同时把附件保存到目录（重名不覆盖）
catmail attachment 7 -o a.pdf   # 保存单个附件（ID 见 `catmail read` 的附件列表）

catmail search 季度报告          # 搜索主题、收发件人和已下载的正文
catmail search from:张三 "quarterly report" -a me@example.com --folder INBOX
catmail gc                      # 回收不再被引用的正文与附件文件

# 写信：先存成草稿，--send 则直接发送
catmail compose --to '张三 <zhang@example.com>, li@example.com' --bcc boss@example.com \
    -s 周报 --body-file report.txt --attach 报表.xlsx
catmail reply 42 --all --body '收到，谢谢！'   # 回复（全部），引用原文
catmail draft list                # 草稿；draft show / edit / attach / detach / rm
catmail send 3                    # 发送草稿 3，并存到"已发送"
catmail outbox                    # 还没发完的邮件；outbox flush / retry / mark-sent / cancel
```

`catmail ls <文件夹>` 的标记列：`*` 未读，`!` 已加星标，`@` 有附件。同步只读（EXAMINE），
不会改动服务器上的任何状态；中途中断（断网、Ctrl-C）后再次运行会从断点继续。

发信先把草稿冻结成 outbox 里的一份快照，再经 SMTP 发出、用 APPEND 存进服务器的"已发送"
（服务器没有这个文件夹就不存）。暂时失败（断网、4xx）会按递增的间隔重试，`catmail outbox`
可以看到原因；服务器永久拒绝（如收件人不存在）的需要用户处理：`outbox cancel` 把它变回草稿
修改后再发，`outbox retry` 原样重发。发出邮件后、收到服务器确认前连接断了的，无法知道对方
是否收到，**不会自动重发**，由用户确认后选择 `mark-sent`、`retry`（可能重复）或 `cancel`。
保存到"已发送"失败只重试保存，不会重发邮件。命令行不在后台发信：`catmail send` 当场发送，
没发出去的用 `catmail outbox flush` 再试；两者在有邮件没发出去时以非零状态退出，便于脚本判断。
`compose` / `reply` 失败（如附件不存在）不会留下草稿。

正文在第一次阅读时下载，阅读不会把邮件标为已读。不超过 2 MiB 的邮件整封下载（附件一并
保存）；更大的邮件只下载正文，附件在第一次打开时再下载，其中超过 2 MiB 的正文部分也作为
附件列出、打开时下载。搜索时中日韩文字按子串匹配，其他文字按整词匹配；正文只有下载过的
才能搜到。

### 图形界面

```sh
catmail-gui                     # 打开窗口；同样支持 --data-dir / CATMAIL_DATA_DIR
```

目前是最小读信窗口：没有账户时先显示登录页（也可以从侧栏的"Add account…"再添加），填写
邮箱、密码和收发服务器；会先实际登录 IMAP 服务器检查设置，成功后才保存并开始同步，密码错误
不会留下任何账户或密钥环条目。之后左侧账户与文件夹，中间邮件列表（方向键切换），右侧读信。只读（写信
在 M5），阅读不会标为已读。默认显示纯文本，有 HTML 时可以切换；HTML 里只显示邮件自带的图片（`cid:`
内嵌图片、`data:`），远程图片一律不加载；排版不对时用"Open in browser"在浏览器里看（同样
拦截远程内容）。点附件可以保存。窗口运行时
CLI 不能打开同一个数据目录（反之亦然），见上文"IPC 待定"。

端口默认按加密方式推断：IMAP `tls` 为 993、`starttls` / `plain` 为 143；SMTP `tls` 为 465、
`starttls` 为 587、`plain` 为 25。

数据目录默认为 Linux `~/.local/share/catmail`、macOS
`~/Library/Application Support/dev.CatMail.CatMail`、Windows `%APPDATA%\CatMail\CatMail\data`，
可用 `--data-dir` 或环境变量 `CATMAIL_DATA_DIR` 覆盖。

Linux 上需要一个 Secret Service 提供者（如 gnome-keyring、KeePassXC）来保存密码；图形界面
需要 Wayland 或 X11，以及 Vulkan 驱动（没有时 wgpu 退回 OpenGL）。

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
`rust-toolchain.toml` 指定的 stable 工具链，Linux 上再装 fontconfig、freetype、libxkbcommon、
libxcb 的开发包（CI 里的包名见 `.github/workflows/ci.yml`）。

图形界面的离屏截图（看 HTML 排版，需要 GPU 或 Mesa lavapipe）：

```sh
CATMAIL_SCREENSHOTS=/tmp/shots cargo test -p catmail-gui -- --ignored screenshots
```

对真实的 IMAP / SMTP 服务器测试（Linux）：

```sh
nix develop .#testenv -c testenv/dovecot/run.sh     # 启动临时 Dovecot（IMAP 10143、提交 10587 / 10465）和 mailpit
# 另一个终端里，先执行它打印的 export，再：
cargo test -p catmail-core --test dovecot --test submission
```

不设置这些环境变量时这些测试自动跳过；其余协议测试用进程内的假服务器，不需要外部依赖。
`dovecot` 测试会让一次 APPEND 因目录只读而失败，所以不能以 root 运行。

CI（GitHub Actions）：

- `ci.yml`：rustfmt 检查；在 Linux、macOS、Windows 上分别跑 clippy（`-D warnings`，包括测试
  代码）。全部测试在 PR 上只在 Linux 跑，合并到 main 后三个平台都跑（私有仓库的 macOS 分钟
  按 10 倍、Windows 按 2 倍计费）；需要时可以在分支上手动触发工作流，在三个平台上跑测试。
  目前只发布 Linux 版，但核心层必须保持跨平台，所以从一开始就在三个平台上检查。
  另有一个 Linux job 对 Dovecot 和 mailpit 跑真实服务器测试。只改文档（`*.md`、`docs/`、`.agents/`、
  许可证）时不触发。
- `nix.yml`：`flake.nix` / `flake.lock` 变更时检查 Nix 格式、`nix flake check`，并确认开发
  环境可用。

| crate | 职责 |
|---|---|
| `catmail-types` | 领域类型，纯数据 |
| `catmail-store` | SQLite、迁移、repository、BlobStore |
| `catmail-proto` | IMAP / SMTP / MIME 适配 |
| `catmail-core` | 引擎与 `MailClient` 门面，前端唯一依赖 |
| `catmail-cli` | `catmail` 命令行 |
| `catmail-gui` | `catmail-gui` 图形界面（gpui-kit） |
| `catmail-testkit` | 测试用的假 IMAP / SMTP 服务器（故障注入），只作为 dev-dependency |

参与开发（包括 AI agent）请先阅读 [AGENTS.md](AGENTS.md)。

## 许可

本项目采用双许可，你可以任选其一：

- Apache License 2.0（[LICENSE-APACHE](LICENSE-APACHE)）
- MIT License（[LICENSE-MIT](LICENSE-MIT)）

除非你明确声明，否则你有意提交给本项目的任何贡献，按 Apache-2.0 许可的定义，都将按上述
双许可授权，不附加任何其他条款或条件。
