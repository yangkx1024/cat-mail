# 日志

`catmail_core::logging`。两个前端在 `main` 一开始调 `logging::init`，装两层输出：

- stderr：按 `RUST_LOG`，默认只有 warn，和以前一样。
- 数据目录下的 `logs/catmail.log`：按 `CATMAIL_LOG`，默认 warn 加上引擎各 crate（core、proto、store）
  的 info，再加上前端自己的那部分：GUI 是 `catmail_gui` 和 `catmail_frontend`，CLI 是 `catmail`。
  `catmail_frontend` 里有常驻时回收内存（trim 前后的 RSS）和没有托盘这两条。

全局 subscriber 由前端显式调 `init` 来装，core 自己从不装：测试在一个进程里开很多引擎。

## 同一时间只有一个进程写

`logging::write_file(&config, frontend)` 打开文件之前先锁住旁边的 `logs/lock`，并且一直持有到进程结束，
所以同一时间只有一个进程在写、在轮转。锁被别的进程占着时什么也不做，事件只到 stderr。GUI 和 CLI 写的是
同一个文件，同一条时间线；每次启动都从下面的文件头开始。

这把锁不用数据目录的锁：引擎打开失败时，数据目录的锁会被放掉，GUI 却还开着、照样在写日志，这时候日志
最有用；另一方面，CLI 也就不必为了写日志先去抢数据目录的锁。

- GUI 在单实例抢到数据目录（`Claim::First`）之后打开。只是把请求交给已在运行的实例的那次启动不写。
- CLI 在打开引擎之前打开。GUI 正在运行时拿不到锁，而引擎反正也打不开，会报"quit it first"。
- 打开之前先像引擎那样建好数据目录、检查它属于当前用户（`config::create_data_dir`）：CLI 这时还没打开
  引擎，第一次运行时数据目录可能还不存在；如果数据目录属于别人，对方可能在里面放了符号链接，把日志
  引到别处去。检查不通过就不写，由随后打开引擎时报错。"其他用户能列出数据目录"的 warn 只在
  `Config::lock` 里打，不会因为两处都建目录而打两遍。
- 打开文件之前的事件（比如单实例交接时的警告）只到 stderr。

## 文件头

每次启动先写两行，发行之后用户发来日志时，不用再追问是哪个版本、跑在什么环境上：

```
INFO catmail_core::logging: started frontend="gui" version="0.1.0" commit="ba094a636c17" profile="release" target="x86_64-unknown-linux-gnu" rustc="rustc 1.98.1 (…)" pid=…
INFO catmail_core::logging: running on os="NixOS 26.11 (Zokor)" kernel="7.2.11-zen1" session="wayland niri" locale="zh_SG.UTF-8" cpus=16 utc_offset=+08 data_dir=~/.local/share/catmail
```

- 版本信息由 `catmail-core/build.rs` 在构建时取得（`version.rs`）：commit 用
  `git describe --always --dirty` 取，有未提交的改动就带上 `-dirty`。
  - 只有 release 构建才去查 git。commit 是编进引擎的，开发时如果每次提交都要重新编译引擎和上面所有
    crate，代价太大；debug 构建因此记为 `unknown`。
  - 没有 `.git` 的构建（源码包、Nix 打包）由打包方设置 `CATMAIL_COMMIT`。
  - build script 只在 HEAD 或分支变化时重新运行，不监听 index；调用 git 时带 `--no-optional-locks`，以免 git
    刷新 index、反过来让 build script 再跑一遍。所以只改了文件时，`-dirty` 可能滞后。
  - `catmail --version` 显示 `catmail_core::VERSION`（`0.1.0 (commit)`），文件头里的是同一个。
- 运行环境（`system.rs`）尽量读文件，启动时不起别的进程：Linux 读 `/etc/os-release` 和
  `/proc/sys/kernel/osrelease`，macOS 读 `SystemVersion.plist`；只有 Windows 起一次 `cmd /c ver`（带
  `CREATE_NO_WINDOW`，GUI 不会闪出控制台）。没有用 `os_info`：它在 Linux 上要起 `lsb_release`、`uname`、
  `getconf` 三个进程，还会带进 `nix`、`objc2`、windows-sys 这些依赖。
- 会话（`XDG_SESSION_TYPE` 加 `XDG_CURRENT_DESKTOP`）和 locale 决定了窗口、托盘、通知和字体最容易出问题的
  地方。文件里的时间是 UTC，`utc_offset` 用来对上用户说的本地时间。
- 不记主机名、用户名。家目录在写文件时统一换成 `~`（`LogFile::write`），所有日志行和 panic 都一样，
  不用每个打日志的地方自己记着处理。

## 写什么

日志点只放在低频的关键路径上：

- 引擎：打开数据目录、每次 IMAP 登录（主机、耗时）、完整同步的汇总（文件夹数、失败数、增删改、
  UIDVALIDITY 重建、耗时）、重放本地改动、已发送和已存入 Sent、IDLE 连接和复用连接断开、睡眠唤醒，
  以及原有的各种 warn。每次 INBOX 同步不记，太频繁。
- 连接问题的出现、变化和恢复（`Engine::report`）：只在前端显示的账户状态变化时记一行，不是每次尝试都记。
  "服务器又能连上了"也在这里记。用户报告的往往就是他们看到的这个状态。
- 连接池（`pool.rs`）：info 只记解释"为什么又登录了一次"的少见事件，keepalive 发现连接已断开（错误、
  NOOP 耗时、连接年龄），丢掉睡前建立的连接，删除账户时关掉的连接数。取用、放回、放不下，以及每次
  keepalive 的 NOOP 往返记在 debug：每条池连接每 4 分钟一次，默认不进文件，排查时用 `CATMAIL_LOG`
  打开（如 `CATMAIL_LOG=info,catmail_core::pool=debug`）。后台同步在两次检查之间的 NOOP 走同一个
  `pool::ping`，记法相同，`whose` 字段区分是池里的连接（`pooled`）还是后台同步的（`background sync`）。
- 按需从服务器取东西（`body::on_server`，正文和附件都走这里）：拿到连接、EXAMINE、FETCH 各用了多久。打开一封
  没下载的邮件慢时，靠它看出慢在哪一步。复用的文件夹 FETCH 落空、重新打开它时另记一行。
- 问服务器的搜索（`search.rs`，见[问服务器](storage.md#问服务器)）：每次搜索每个账户 info 一行
  `searched the server`：搜了几个文件夹（共几个）、服务器命中几封（`matched`，正文在不在都算）、补到几封
  （`found`）、用的语法（`TextSearch`：`Keys` 即 `TEXT`、`GmRaw`、`QqMail`，不能按文字搜的为 `Unsupported`）、有没有带 `CHARSET UTF-8`、词数、EXAMINE 了几个
  文件夹（`opened`，连接上已打开着的不再 EXAMINE）、EXAMINE 和 SEARCH 各花的总时间、是否被放弃、出错时的错误。不记搜索词本身。`matched` 为 0 而本地明明有这段正文，说明服务器不支持这种
  搜索（比如中文不按子串）；`matched` 大而 `found` 为 0，是命中的正文都已在本地。服务器拒绝搜索时（如
  `BADCHARSET`）另记一行带服务器原话；服务器不能按文字搜（Coremail，或无视文字、全部命中）时记一行
  `the server can't search text`，之后不再问。每个文件夹的命中数和两段耗时、跳过的文件夹（UIDVALIDITY 变了、打不开）、
  因登录被拒或超限不问的账户记在 debug：`CATMAIL_LOG=info,catmail_core::search=debug`；提前连接
  失败记 info 一行（连上的是通常的 `logged in to IMAP`）。GUI 发起搜索或提前连接失败时 warn 一行。
- 退出：GUI 退出时记一行 `quitting reason=…`，退出被取消时记 `quit called off`（见[后台运行](background.md#退出)），
  `MailClient::shut_down` 记 `shutting down the engine`，引擎关闭时 core 记 `engine closed`；`catmail watch` 被 Ctrl+C 中断时记 `interrupted; stopping`。日志停在别处，
  就是进程被杀或崩溃了。
- GUI：打不开邮件、附件、图片缓存时的错误（warn）；主窗口开关、进入常驻、trim 前后的 RSS
  （见[后台运行](background.md#引擎属于应用不属于窗口)）。
- panic 连同 backtrace 直接写进文件，不经过 `tracing`，以免 `tracing` 自己 panic 时又回到这里。

不记密码和邮件内容。错误信息会引用服务器的原话，账户相关的问题里可能出现邮箱地址，所以文件和目录与
数据目录里的其他文件一样只有本用户可读（0600 / 0700）。

## 文件

- 按大小轮转：超过 8 MiB 时改名为 `catmail.1.log`（覆盖上一个），再从头写，两个文件合计约 16 MiB。
  常驻的应用可能一跑几周，按日期轮转没有上限，所以没有用只按时间轮转的 `tracing-appender`。
- Windows 上改名前先关掉文件。改名失败（别的程序开着它）时就地清空重写。
- 每个事件一次写，不带缓冲：崩溃前的最后几行最有用。
