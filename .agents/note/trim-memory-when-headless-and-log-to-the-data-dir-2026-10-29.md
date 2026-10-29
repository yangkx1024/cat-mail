# 常驻时把空闲内存还给系统，日志写进数据目录

- 日期：2026-10-29
- 类型：功能

## 背景

在 release 构建上量内存：关掉主窗口、切到 headless 之后 RSS 还有约 227 MB，比有窗口时只少约 34 MB。
用 gdb 调 `malloc_info` 发现 glibc 持有约 103 MB 堆，其中约 92 MB 是空闲块（主堆 49 MB 里 46 MB 空闲，
一个线程 arena 28 MB 几乎全空），真正在用的只有约 11 MB：窗口释放的内存夹在仍在用的块之间，glibc 只能
从堆顶往回缩。手动调一次 `malloc_trim(0)`，RSS 降到约 151 MB。

同一次排查里还碰到一个实例跑在 llvmpipe（CPU 渲染）上，RSS 460 MB；重启后正常选中 RADV。GPUI 只在
info 级别说为什么跳过了显卡，而 GUI 默认只输出 warn，原因已经查不到。所以要有一份常驻期间也在写的
日志。

## 决策

- 只在切到 headless 成功之后 trim 一次，此时窗口相关的东西都已释放。不在每次关窗口、每次同步之后做：
  重新缺页的代价（80 MB 约 2 万次缺页，几十毫秒）只该在重开窗口时付一次。只在 Linux glibc 上做；
  其他平台的分配器会自己归还。考虑过 `M_ARENA_MAX` 和换 mimalloc：前者（`mallopt`）只限制线程 arena 的数量，
  而大部分空闲内存在主堆里，只有 trim 的 `madvise` 能还回去；后者是新的全局分配器，影响三个平台，收益要另外测。
- `malloc_trim` 要 `unsafe`。工作区的 `unsafe_code` 从 `forbid` 改成 `deny`，只在 `memory` 模块里放行，
  其余 crate 在根上 `#![forbid(unsafe_code)]`，守住原来的规矩。
- 日志文件放在 core（`catmail_core::logging`），GUI 和 CLI 共用：打日志的关键路径大多本来就在引擎里，
  数据目录的规则（放哪、谁能写、权限）也该由 core 管。最初放在 GUI 里，CLI 跑 `catmail sync` 时就没有
  文件日志，所以改了。全局 subscriber 仍由前端显式调 `logging::init` 来装，core 自己从不装：测试在一个
  进程里开很多引擎。
- 同一时间只有一个进程写：日志文件自己持有 `logs/lock`，一直到进程结束。最初是凭 `DataDirLock` 打开的，
  但引擎打开失败时数据目录的锁会被放掉，GUI 却还在写，所以改了；CLI 也就不必为了日志先去锁数据目录。
  两个前端写同一个 `logs/catmail.log`，同一条时间线。打开之前先像引擎那样建好数据目录并检查属主：CLI 在
  打开引擎之前就开日志，第一次运行时数据目录可能还不存在；属于别人的数据目录里可能放了符号链接。
- 没有用 `tracing-appender`：它只按时间轮转，常驻应用要的是总量封顶。改为按大小轮转：超过 8 MiB 改名
  为 `catmail.1.log`，合计约 16 MiB。
- 文件默认级别是 warn，加上引擎各 crate 的 info，再加上前端传进来的部分（GUI 含 `gpui_wgpu`）；可以用
  `CATMAIL_LOG` 覆盖。stderr 仍按 `RUST_LOG`，默认 warn。
- 家目录在写文件时统一换成 `~`，所有日志行和 panic 都一样，不靠每个打日志的地方自己处理。
- 连接问题的出现、变化和恢复在 `Engine::report` 里记，只在账户状态变化时记一行。
- 每次启动先写文件头：版本、commit（`build.rs` 在 release 构建时用 `git describe` 取，没有 `.git` 时由
  `CATMAIL_COMMIT` 提供；debug 构建不查，免得每次提交都重新编译引擎和上面所有 crate）、
  profile、target、rustc、pid，以及系统和版本、内核、桌面会话、locale、CPU 数、UTC 偏移、数据目录
  （家目录换成 `~`）。为了发行之后分析用户发来的日志。系统信息自己从文件里读，没有用 `os_info`：它在 Linux
  上要起三个进程，依赖也多。`--version` 同时显示 commit。
- 日志点只加在低频的关键路径上：启动、引擎打开、IMAP 登录、完整同步汇总、重放本地改动、发送和存入
  Sent、主窗口开关和显示连接切换、trim。IDLE 断线和复用连接失效从 debug 提到 info，排查"为什么没收到
  新邮件"时最先要看它们。每次 INBOX 同步不记，太频繁。

## 方案

- `catmail-core/src/logging.rs`：`init(frontend_filter)` 装 stderr 和文件两层；`write_file(&lock, frontend)`
  打开 `Config::log_dir()`（新增，`<data_dir>/logs`）。文件 0600、目录 0700。每个事件一次不带缓冲的写；
  panic 连同 backtrace 直接写进文件，不经过 `tracing`。前端不再依赖 `tracing-subscriber`。
- `catmail-core/build.rs`、`version.rs`（`VERSION` 及构建信息）、`system.rs`（运行环境，带单测）。
- `catmail-cli/src/main.rs`：打开引擎之前开日志文件。`catmail-store` 公开 `private_file_options`，store
  自己和日志文件共用 0600 的规则。
- `catmail-gui/src/memory.rs`：`release_freed`，trim 并记下前后的 RSS；`Resident` 切到 headless 成功后
  在后台线程调用。主窗口开、关和显示连接的切换也记进日志。
- core：`client.rs` 引擎打开；`connection.rs` 登录（主机、耗时）；`sync.rs` 完整同步的汇总；`ops.rs`
  重放；`send.rs` 已发送、已存入 Sent。
- 文档：新增 `docs/architecture/logging.md`；`architecture.md` 的数据目录表加上 `logs/`；`background.md` 记下 trim。
- 验证：`logging` 的单测覆盖轮转、续写已有文件、超长事件、一次只有一个进程写、家目录换成 `~`、文件权限、
  新数据目录、属于别人的数据目录（后两个去掉修复后会失败）。连续两次 release 构建，第二次 core 是 Fresh
  （仓库里没有 `packed-refs` 时，监听这个不存在的文件曾让 build script 每次都重跑）。用临时数据目录跑 release 版的
  GUI 和 CLI，日志里有两者的启动行、引擎打开和 GPU 选择。trim 的效果是在正在运行的实例上用 gdb 手动调用量出来的。
