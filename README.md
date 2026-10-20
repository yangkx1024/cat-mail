# cat-mail

用 Rust 编写的跨平台邮件客户端。核心是一个与 UI 无关的邮件引擎（账户、收件箱、草稿、发件、
附件），对外只暴露 `MailClient` 门面；在它之上有两个平级的前端：

- **`catmail`**：命令行，可直接在终端使用，也支持 `--json` 供脚本调用
- **`catmail-gui`**：图形界面，基于 [GPUI](https://www.gpui.rs/) 与 [gpui-kit](https://github.com/longbridge/gpui-kit)（目前只有最小读信窗口）

数据存储在本地 SQLite，离线可用；密码保存在系统密钥环中，不进数据库。

> 项目处于早期开发阶段（M4 已完成，下一步是 M5 的图形界面），接口和数据库结构仍会变化。

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
| M4 | 离线操作队列（标记 / 移动 / 删除）；IDLE 推送与后台同步；草稿同步到服务器 | ✅ 完成 |
| M5 | 图形界面：三栏布局、读信、写信 | |
| M6 | OAuth2（Gmail / Microsoft 365）；Windows / macOS 打包 | |

几个安排上的考虑：

- **最小读信窗口放在 M2。** gpui-kit 的 HTML 渲染定位为简单 HTML，而真实邮件大量使用表格
  布局和内联样式。一拿到正文就用真实邮件验证表格、`cid:` 内嵌图片和中文排版，不等到 M5。
  结果：段落、数据表格、内嵌图片和中文排版正常；用表格做布局的邮件（多数营销邮件）原本会被
  压成一段，现在渲染前先把布局表格展开成块（见 [docs/architecture/gui.md](docs/architecture/gui.md)），"在浏览器打开"作为兜底。
- **协议测试环境从 M1 开始建。** 一个可注入故障的进程内假 IMAP 服务器（`catmail-testkit`，
  三个平台的 CI 都跑），覆盖断线、UIDVALIDITY 改变、重复或乱序响应、服务器缺少 CONDSTORE、
  STARTTLS 明文注入、同步中途中断后续传；再加一个真实的 Dovecot（Nix 提供，不需要 docker）。
  M3 加入了假 SMTP 服务器，覆盖 SMTP 确认丢失、连接中断、收件人被拒、保存到 Sent 失败和
  APPEND 答复丢失等场景；发信也对真实服务器测试：Dovecot 的 submission 服务（STARTTLS、
  隐式 TLS、AUTH、SIZE），转发给 mailpit 核对收到的内容。
- **草稿同步放在操作队列之后。** 草稿每次修改都要在服务器的"草稿箱"里换一份副本，删除旧副本
  正好经过操作队列（核对 UIDVALIDITY、离线可用），所以 M3 的草稿只在本地，M4 才同步。目前只单向
  上传：其他客户端写的草稿在"草稿箱"里显示为普通邮件，不会变成可编辑的本地草稿。
- **IPC 待定。** 同一个数据目录同时只允许一个引擎（见架构文档），所以目前 GUI 运行时 CLI
  不能打开同一个数据目录。之后让其他前端通过 IPC 连接正在运行的引擎。

## 使用

目前可以管理账户、同步文件夹和邮件头、列出和阅读邮件、搜索，写信、回复、转发和发信，以及标记、
移动和删除邮件：

```sh
# 添加账户（交互输入密码；脚本里用 --password-stdin）
catmail account add --email me@example.com --name "Me" \
    --imap-host imap.example.com --smtp-host smtp.example.com --smtp-security starttls

catmail account list            # 加 --json 输出 JSON
catmail account remove me@example.com

catmail sync                    # 同步所有账户；也可以指定 id 或邮箱
catmail watch                   # 保持同步直到 Ctrl-C，打印变化（见下文）
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
catmail forward 42 --to li@example.com --body '转给你看看'   # 转发，带上原信的附件
catmail draft list                # 草稿；draft show / edit / attach / detach / rm
catmail send 3                    # 发送草稿 3，并存到"已发送"
catmail outbox                    # 还没发完的邮件；outbox flush / retry / mark-sent / cancel

# 标记、移动、删除：先改本地，再当场同步到服务器
catmail mark 42 43 --read --flag  # 也有 --unread / --unflag
catmail mv 42 --to Archive
catmail rm 42                     # 移到"已删除"；已在其中或加 --permanent 则永久删除
catmail ops                       # 还没同步到服务器的改动，以及被拒绝的；ops flush / dismiss
```

`catmail watch` 在前台持续同步所有账户：收件箱的新邮件、删除和标记变化由服务器推送（IDLE，不支持时
每 2 分钟检查一次），其他文件夹每 15 分钟同步一次，标记、移动、删除立即同步到服务器；断线后自动重连，
连不上或恢复时打印一行。登录被拒绝时每 15 分钟才再试一次（反复失败的登录会让服务器锁定账户）。
它占用数据目录，运行期间不能再用别的 `catmail` 命令，所以它每 6 小时自己回收一次不再引用的文件
（同 `catmail gc`）。

`catmail ls <文件夹>` 的标记列：`*` 未读，`!` 已加星标，`@` 有附件。同步本身只读（EXAMINE），
中途中断（断网、Ctrl-C）后再次运行会从断点继续。

标记、移动、删除离线也能用：改动立刻反映在本地（移动的邮件马上出现在目标文件夹，已下载的
正文随之保留），同时记入队列，`catmail mark` / `mv` / `rm` 随即把它同步到服务器；连不上时
改动保留，由下一次 `catmail sync` 或 `catmail ops flush` 完成（这时命令以非零状态退出）。
服务器拒绝的改动会在本地撤销，`catmail ops` 可以看到原因。文件夹在服务器上被重建
（UIDVALIDITY 改变）时，还没完成的改动作废，以服务器为准。回复发出后原信标记为"已回复"。

草稿同时保存在服务器的"草稿箱"里（账户有这个文件夹时），每次修改换成新的一份，删除草稿或发出后
副本随之删除。`compose` / `reply` / `forward` / `draft edit` 等命令当场上传；连不上时草稿照样保存在本地，只给出
警告，由下一次 `catmail sync` 或 `catmail ops flush` 上传。`catmail draft list` 的 SERVER 列显示
服务器上是否已是最新版本，服务器拒绝保存（如超出配额）时 `catmail draft show` 显示原因。后台同步
（图形界面）等草稿停止修改 15 秒后才上传，边写边自动保存不会每次都换一份副本。

发信先把草稿冻结成 outbox 里的一份快照，再经 SMTP 发出、用 APPEND 存进服务器的"已发送"
（服务器没有这个文件夹就不存）。暂时失败（断网、4xx）会按递增的间隔重试，`catmail outbox`
可以看到原因；连不上服务器或登录被拒绝（如密码错误）时，排队的邮件一起等待，不算作哪一封的尝试；服务器永久拒绝（如收件人不存在）的需要用户处理：`outbox cancel` 把它变回草稿
修改后再发，`outbox retry` 原样重发。还在排队的邮件随时可以 `outbox cancel`，不必等正在发的那封。发出邮件后、收到服务器确认前连接断了的，无法知道对方
是否收到，**不会自动重发**，由用户确认后选择 `mark-sent`、`retry`（可能重复）或 `cancel`。
保存到"已发送"失败只重试保存，不会重发邮件。命令行不在后台发信：`catmail send` 当场发送，
没发出去的用 `catmail outbox flush` 再试；两者在有邮件没发出去时以非零状态退出，便于脚本判断。
`compose` / `reply` / `forward` 失败（如附件不存在、转发时原信的附件下载不了）不会留下草稿。

正文在第一次阅读时下载，阅读不会把邮件标为已读（用 `catmail mark --read`）。不超过 2 MiB 的邮件整封下载（附件一并
保存）；更大的邮件只下载正文，附件在第一次打开时再下载，其中超过 2 MiB 的正文部分也作为
附件列出、打开时下载。搜索时中日韩文字按子串匹配，其他文字按整词匹配；正文只有下载过的
才能搜到。

### 图形界面

```sh
catmail-gui                     # 打开窗口；同样支持 --data-dir / CATMAIL_DATA_DIR
```

目前是最小读信窗口：没有账户时先显示登录页（也可以从侧栏的"Add account…"再添加），填写
邮箱、密码和收发服务器；会先实际登录 IMAP 服务器检查设置，成功后才保存并开始同步，密码错误
不会留下任何账户或密钥环条目。之后左侧账户与文件夹，中间邮件列表（方向键切换），右侧读信。窗口打开期间在后台保持同步，新邮件
由服务器推送，与 `catmail watch` 相同，并且同样定期回收不再引用的文件。只读（写信
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
  Linux 上的测试同时对 Dovecot 和 mailpit 跑真实服务器测试（服务器来自 flake 的 `testenv`，
  cargo 仍用 job 自己的工具链，复用同一份编译缓存）。只改文档（`*.md`、`docs/`、`.agents/`、
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
