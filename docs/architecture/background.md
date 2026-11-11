# 后台运行

[← 架构](../architecture.md)

> 状态：常驻与单实例唤起、引擎自己发现睡眠、GUI 听系统唤醒、新邮件事件、Linux 的系统通知与托盘、macOS 的状态栏、`--background`、
> macOS 的开机启动已实现。macOS 的新邮件通知、Linux 的开机启动尚未实现；Windows 还没有 GUI（见 [GUI](gui.md)）。
> 没实现的小节是设计。

GUI 关掉主窗口后继续同步、发信、提醒新邮件，并能从托盘、再次启动或通知回到窗口。

## 进程模型：GUI 进程常驻

关掉主窗口后 GUI 进程不退出，只释放窗口；引擎、托盘、通知都在这个进程里。

考虑过"守护进程持有引擎、GUI 经 IPC 连接它"，没有采用，按可维护性也不更好：

- `MailClient` 的六十多个 async 方法、事件流、文件交接都要过 IPC，两个进程的版本要配对。
- 守护进程省不掉 UI 循环：macOS 的状态栏图标要 NSApplication 主线程，它实际上还是一个 UI 进程。
- 常驻 GUI 的开销接近守护进程：窗口、它的视图线程和 WebKit 渲染进程都随窗口退出，Linux 上的托盘
  （StatusNotifierItem）和通知都走 D-Bus。⚠️ GTK 常驻时仍连着显示服务器（GPUI 版能断开），实测见下。

守护进程真正的好处是 CLI 能与 GUI 同时使用，这留给"引擎 IPC"（见[与 CLI 的关系](#与-cli-的关系)）。
常驻的部分集中在外壳（`catmail-gui/src/shell/`）和 `catmail_frontend::resident`，以后要拆成独立进程时整体搬出去。

## 两个状态

```
有窗口 ──最后一个窗口关闭，且开着"关闭窗口后保持运行"──▶ 常驻
  ▲                                                    │
  └──── 托盘 / 再次启动 / 点击通知 / Dock ───────────────┘
有窗口、常驻 ──Quit（Ctrl Q、托盘菜单、macOS 应用菜单）──▶ 退出
```

- 何时退出由外壳决定，不是工具包：GTK 的应用整个运行期间 `hold()` 着，否则它随最后一个窗口结束；AppKit 的
  `applicationShouldTerminateAfterLastWindowClosed` 答否。外壳在窗口没了时（`window_removed`）决定，要退出时
  `toolkit::end`。
- 设置"关闭窗口后保持运行"（`settings.json`）默认开。关闭时最后一个窗口关掉就退出。默认开，是因为没有托盘的
  桌面（GNOME 未装 AppIndicator 扩展）也能从启动器再点一次回到窗口（见[单实例与唤起](#单实例与唤起)），不会
  "找不回来"。
- 写信窗口也算窗口：关掉主窗口时还有写信窗口开着，就等它们都关掉才进入常驻。
- macOS 上常驻时切到 Accessory 的激活策略（`toolkit::resident`）：没有 Dock 图标，不在应用切换器里，只留状态栏
  图标；显示窗口时（`Window::present`）切回 Regular 并激活应用。`--background` 启动一开始就是 Accessory，Dock 图标
  不先闪一下；登录时的启动要到启动完成才知道，bundle 的 Dock 图标会短暂出现。常驻时从 Finder、启动台再打开仍经
  `applicationShouldHandleReopen` 回到窗口；Dock 角标照旧设置，回到 Regular 时就在。

## 引擎属于应用，不属于窗口

引擎由应用持有（`catmail-gui/src/engine.rs`）：打开中、已打开（`MailClient`）、打开失败或已关闭。窗口来来去去，
引擎一直运行到应用退出；视图用 `engine::opened` 等它打开。外壳（`shell/`）另外记着主窗口、写信窗口、托盘和通知
对主窗口的请求（`Ask`），以及主窗口上次的位置。

不依赖窗口的事件订阅在引擎打开时启动：新邮件通知（`new_mail.rs`，见[新邮件](#新邮件)）、托盘（`tray/`，见
[托盘](#托盘)）。它们不存 `MailClient`：启动时订阅一次事件，之后每次用到都从 `engine::open` 取（引擎没打开或已在
退出时是 `None`），退出后也就不再发出注定得到 `Error::Closed` 的调用。引擎关闭时事件流结束，它们的循环随之停下。

主窗口关闭时它的视图和视图线程随之结束：列表、读信区、图片都释放，WebKit 的渲染进程也退出。重新打开时新建
邮箱，外壳只记住上次选中的 `Place`（`shell::left_at`），那里已经不在就回到第一个收件箱。期间托盘、通知的请求
（去某处、写信）记着，新的邮箱打开时接过去（`follow_asks`）。

最后一个窗口关闭时怎么做是一个纯函数（`catmail_frontend::resident::after_last_window`，退出原因 `QuitReason` 也在
那里）：正在退出、没开"保持运行"、引擎打开失败（没有东西可运行）时退出；否则常驻。常驻 2 秒后（视图线程放掉
它们持有的东西之后）在另一个线程上调一次 glibc 的 `malloc_trim(0)`（`catmail_frontend::memory::release_freed`，只在
Linux glibc 上，其他平台什么也不做）：glibc 的堆只能从顶上缩，释放的内存夹在仍在用的块之间，留在进程里。
`unsafe_code` 因此在 frontend 里是 `deny` 而不是 `forbid`，由 `memory` 模块单独放行（还有 macOS 的状态栏，见
[托盘](#托盘)）。

实测（release，两个真实账户，Linux），单位 MB，RSS / PSS：

| | 主进程 | WebKit 渲染进程 | WebKit 网络进程 | 合计 PSS |
|---|---|---|---|---|
| 刚启动、还没开邮件 | 226 / 193 | — | — | 193 |
| 开着窗口读信 | 280 / 192 | 195 / 107 | 69 / 32 | 约 335 |
| 关窗后、trim 后 | 265 / 197 | 退出了 | 69 / 41 | 约 238 |

trim 只还回约 14 MB。常驻时主进程里约 107 MB 是文件映射（程序本身和 GTK、WebKitGTK 的库），能和别的进程共用，
也能被换出。WebKit 的网络进程从不联网（CSP 只放行 `data:` 和读信区自己的 scheme），表里它却一直留着：当时 nsh 一直持有
`NetworkSession`。nsh 已经改成随最后一个 web view 结束它（longbridge/nsh#1051），workspace 也已升级到含这个修复的
版本；表里的数字是修复之前量的，在 Linux 上重新量过之前先这样（见 `crates/catmail-gui/ROADMAP.md`）。

打开邮箱时不做启动同步（后台同步打开时就完整同步一遍），重开窗口不会多出同步；打开时查一次被拒绝的改动，
正好报告常驻期间被拒绝的。

## 单实例与唤起

常驻之后，从启动器再点一次 catmail 应当唤起已有实例的窗口，而不是显示 "Couldn't open the mail
data"。macOS 上 LaunchServices 把同一个 bundle 的再次打开交给已在运行的那个，点 Dock 图标经
`applicationShouldHandleReopen` 唤回主窗口；但 `cargo run`、命令行直接启动的不经过它，Linux 上 GTK 应用也是
`NON_UNIQUE`（不经 D-Bus 判断唯一），所以自己做，三个平台一样：

- **端点是数据目录里的 Unix socket `gui.sock`**（`catmail-frontend/src/instance.rs`），由持有数据目录锁的
  进程监听：目录只有本用户可访问，socket 本身也设成 0600。Windows 10 起也有 AF_UNIX，std 不支持，用 `uds_windows`（zbus 已经
  带进依赖树），三个平台一套代码；没有选 named pipe，那要自己设 DACL。
- **路径太长时放进运行时目录。** socket 地址最多约 100 字节（`sun_path`：Linux、Windows 108，macOS 104），
  数据目录的路径太长（例如很深的 `--data-dir`）时绑定会失败，于是改放在用户自己的运行时目录：Linux 上是
  `$XDG_RUNTIME_DIR`（经 `directories`；没有它就不放，绝不放进别人也能写的 `/tmp`），其他平台是每个用户的
  临时目录。文件名取数据目录规范路径的 FNV-1a 哈希：不用 Rust 的 `DefaultHasher`，它在不同版本间可能变，
  而升级后的新版本要找得到还在运行的旧版本。放哪里也按规范路径（`dunce::canonicalize`，Windows 上不带
  socket 路径用不了的 `\\?\` 前缀）的长度决定：否则一个启动用很长的真实路径（socket 进运行时目录）、
  另一个用指向它的短链接（找 `链接/gui.sock`），两者争同一把锁却找不到对方。
- **谁是实例由引擎的锁决定**，在工具包的应用启动之前，不会闪出窗口：
  1. 对数据目录加引擎的锁（`Config::lock`）。拿到了，这个启动就是实例：删掉残留的 socket、绑定、开线程
     监听，再把锁交给引擎（`MailClient::builder_locked`），引擎打开时不再加锁。锁在 `main` 里同步地拿，
     迁移仍在后台：其间再启动的那个已经连得上 socket。
  2. 锁被占用：向 socket 发 `Activate { display }`，得到答复就退出。
  3. 连不上：可能另一个启动正在加锁和绑定之间，每 50 ms 重试，最多 1 秒；仍然不行就是不听 socket 的
     进程占着锁（CLI），照常启动，打开引擎时显示"数据目录被占用"。
  4. 绑定失败只记日志，引擎照常打开；之后的启动走第 3 步，显示"数据目录被占用"。
  - 起初 GUI 另有一把 `gui.lock`，因为引擎在 `open_async` 里才加锁；改为让核心单独提供加锁，只剩一把锁，
    也不用对外公开创建数据目录的规则。
- **协议**：一行一个 JSON 请求、一行答复，带版本号；版本不同时答复说明原因，而不是解析失败。先只有
  `Activate { display }`，`display` 是请求方的 `WAYLAND_DISPLAY`、`DISPLAY`、`XDG_RUNTIME_DIR` 和
  `XDG_ACTIVATION_TOKEN`（只在 Linux 上有）。以后加 `Compose { mailto }`（注册为默认邮件程序时用）、
  打开某封邮件，也可以长成"其他前端经 IPC 连接正在运行的引擎"。
- 收到 `Activate`：打开主窗口（没有时）并放到前面，用请求方的激活令牌，Wayland 上它才能获得焦点。常驻时 GTK
  仍连着原来的显示服务器，不按请求方的显示环境重连。
- 监听在一个专门的线程上（阻塞的 accept），请求经 channel 交给主循环上的任务。

## 系统睡眠与唤醒

`Engine::resume` 有两个来源：前端知道系统醒了时调 `MailClient::resume()`；引擎自己发现睡眠（见下）。nsh 不转告
系统唤醒，GUI 自己听（`catmail-gui` 的 `wake.rs`，引擎打开后开始，直到退出）：Linux 上是系统总线上 logind 的
`PrepareForSleep(false)`（GIO 的 D-Bus，在 GTK 的主循环上；systemd 和 elogind 都有，Flatpak 里要
`--system-talk-name=org.freedesktop.login1`），macOS 上是 `NSWorkspace` 通知中心的
`NSWorkspaceDidWakeNotification`。两边都只是调 `resume`；听不到（没有系统总线、没有 logind）时还有引擎自己发现。
`catmail watch` 只靠后者。常驻之后这是最容易出问题的一处：

- Linux、macOS 上 `Instant` 和 tokio 的定时器不计睡眠时间，醒来后下一次完整同步最多还要再等 15 分钟；
  发件箱等重试的定时器同样晚到。
- 后台任务醒来后接着用 IDLE 那条连接，它多半已经断了，要等超时或 keepalive 才发现。

`Engine::resume` 只记下醒来的时刻（`woke`），哪些东西过时由各处用到时比较（`Engine::since_woke`）：
睡前做的事都记着自己从什么时候开始，早于这个时刻的不再算数。比"醒来就清空"更准：清空只管当下，
跨过这一刻、醒来后才做完的事会把过时的结果再记回去。

- **连接**：每条连接记着它建立的时刻（`Session::opened`）。要在一条连接上做事时（`on_session`，
  连接池和后台任务都经过它）换掉睡前建立的，不论它来自池里、是醒来时正被前台操作用着又放回去的，还是
  后台任务手上的。池里连接的 keepalive 定时器按 `Instant` 算，单靠它看不出睡过。
- **同步时间**（`synced`、`folder_synced`）：记下时带上这次同步开始的时刻，开始于睡前的不记，睡前记下
  的读出时当作没有。按不计睡眠的时钟算出的"刚同步过"不说明什么；睡前开始、连接撑过睡眠做完的同步，
  看到的也可能是睡前的服务器。于是完整同步到期，打开文件夹时也会同步（见
  [同步 / 打开的文件夹](sync.md#打开的文件夹)）。
- **叫醒后台任务**（每账户的 `resume` 信号，与 `wake` 分开）：信号只叫它马上看一眼，取舍仍按上面的比较，
  多叫一次不会多做。IDLE 中的连接若是睡前建立的就不发 DONE，直接丢掉（DONE 的答复可能永远不来），
  醒来后建立的接着 IDLE；等待（轮询、重试间隔）中的立即醒来。一步正在进行时信号留到它下次等待：
  那一步要么在醒来后做完（中途连接断了就在新连接上接着做），要么等超时失败。
- 最后唤醒发件箱，它按墙上时钟重新算哪些到期。

从真正醒来到 `resume` 被调用之间开始的事，会被当成睡前的重做一次：引擎只能从这个调用知道什么时候醒的。

"登录被拒绝"的账户收到信号也不理会，照旧等它的间隔（每次重试都是一次失败的登录，见
[同步 / 后台同步](sync.md#后台同步与-idle)）。超出下载限额的账户不同，醒来就去试：它的一小时等待由 tokio
的定时器计，没算上睡着的时间。

醒来时网络常常还没连上（DNS 解析失败），要再过几秒才好。连不上服务器时，醒来后的失败先从 2 秒起
翻倍重试，到平常的第一次间隔（30 秒）为止，即 2、4、8、16 秒，之后照常从 30 秒起翻倍；睡前的失败不计入，
它们说明不了醒来后的网络。成功一次就回到平常的间隔，之后的失败照旧等。只在引擎里，三个平台相同，
不用新依赖。

- 计数在 `Engine::report` 里，和账户状态的失败次数记在一起，每个服务器（同步的 IMAP、每个身份的 SMTP）
  各一份，`report` 返回两者（`Failures`），等多久由 `Failures::retry_in` 统一算：后台同步
  （`SyncSchedule::retry_after_wake`）和发件箱连不上服务器时（`send.rs`）都用它。醒没醒过与连接、同步
  时间一样，用 `since_woke` 比较上次记录的时刻。不改 `ServerProblem::failures`：那是前端显示的。
- 只管连不上服务器：发件箱里某一封连上之后的暂时失败按它自己的尝试次数退避，与睡眠无关。**登录被拒绝**
  算连上了服务器，结束快速重试：网络已经好了，再快速重试只会多几次被拒的登录，服务器累计几次就锁定账户
  （IMAP 照旧等 `full_sync`，SMTP 照常从 30 秒起）。
- 快速重试的间隔也不超过最长间隔（`full_sync` / 30 分钟），即使它比平常的第一次间隔还短。

- 没有监听网络变化（NetworkManager、`nw_path_monitor`、`INetworkListManager`）：每个平台各写一份，
  Linux 上还不一定有 NetworkManager；网络连上了也不等于服务器连得上，失败了仍要重试。头 30 秒里多试
  几次的代价只是几次很快失败的连接。
- `MailClient::resume` 记一条日志（"the frontend says the computer woke"），引擎自己发现睡眠时记的是
  "the computer slept"：从日志能看出唤醒是谁、在哪一刻处理的。

### 引擎自己发现睡眠

`catmail watch` 和以后的前端不必各自监听系统唤醒：引擎在它存在期间另起一个任务（`sleep.rs`），
每 30 秒比较墙上时钟和 `Instant` 各走了多少。Linux、macOS 上前者计入睡眠、后者不计，
墙上时钟多走了 60 秒以上就算睡过，调用同一个 `Engine::resume`。

- 记下的醒来时刻是发现的那一刻，最多晚 30 秒（tokio 的定时器也不计睡眠，醒来后接着走完剩下的间隔），
  这中间开始的事重做一次：偏保守，不会出错。前端若能在醒来那一刻调 `resume`，就不必等这 30 秒。
  上次检查之后已经有人调用过 `resume`，任务就不再调用：再调用一次会把醒来时刻往后挪，丢掉这之间新开的
  连接、作废这之间的同步，再来一遍。
- 墙上时钟本身会跳：校时、用户往前调时间都会误报，代价是换一次连接、完整同步一次。60 秒的阈值远大于
  漂移和通常的校时；更短的睡眠发现不了，连接多半撑得过，定时器也只晚这么多。墙上时钟往回调不算。
- 不用计入睡眠的时钟（`CLOCK_BOOTTIME`、`mach_continuous_time`）：有了 `resume` 的比较，过时的同步时间
  和连接都已处理，与时钟计不计睡眠无关；tokio 的定时器和卡在死连接上的 IDLE 也要靠外部信号打断。
- Windows 上 `Instant` 可能计入睡眠，那样两边走得一样，发现不了；以后的 Windows GUI 要在收到系统唤醒时
  `resume`。
- 不论前端开没开后台同步、发信都起这个任务：连接池里的连接同样会睡过期，`resume` 的作用在引擎，不在
  某个后台任务。代价只是每 30 秒一次定时器。

## 新邮件

通知要知道哪些是"新到的"邮件，这由引擎判断（规则在引擎，措辞在前端）：
`Event::NewMail { account, messages }`，同步拉取新邮件的每一批提交之后、紧跟那一批的
`MessagesChanged` 发出（`sync::fetch_new`；哪些行算新邮件由 `Messages::new_mail` 在同一个事务里挑出）。

- 只算同一 UIDVALIDITY 下、UID 不小于同步前游标里 UIDNEXT 的邮件，也就是新到达的，不算补拉的旧邮件。
  首次同步、UIDVALIDITY 改变、同步中断后没有游标时都不算，重建数据库后的全量同步不会弹出几千条通知。
- 只算这次新插入的行：Gmail 上被别的客户端从已删除移回收件箱的邮件在"所有邮件"里有新 UID，但它的行
  是移过来的，不算。同步中断后重来时已经存下的行不再插入，同一封不会报两次。
- 只算 INBOX；Gmail 上是"所有邮件"里带 INBOX 标签的（见[位置与标签](labels.md)）。
- 只算未读的：在别的设备上已经读过的不提醒。
- 已知的误报：服务器不给 COPYUID 时，本地移进 INBOX 的未读邮件回放后行被删掉、由同步重新拉取
  （见[操作队列 / 回放](op-queue.md#回放)），会被当成新邮件。支持 MOVE 的服务器几乎都给 COPYUID。

`catmail watch` 为它打印一行"N new messages"（`--json` 是 `new_mail` 事件，带邮件 id）。

### 系统通知

GUI 的 `new_mail.rs` 在引擎打开时订阅事件，有窗口时、常驻时都在。攒、措辞、每条通知的 tag 和点击后的去处不依赖
UI 库，在 `catmail_frontend::new_mail`（`Arrivals`、`notifications`、`Target`）：

- **先攒 1 秒再说**（`GATHER`）。一次同步按批提交、每批一个 `NewMail`，醒来后各账户又同时同步；不攒的话一阵
  新邮件会弹出好几条。同一个 tag 的通知替换前一条（`send_notification` 的 id）。
- **主窗口在前台时也说**：不看窗口在不在前台、开没开着。
- **说之前重读**：`MailClient::unread_in_inbox` 用一条查询挑出各账户仍未读、仍在 INBOX（Gmail：仍带 INBOX 标签）
  的，与 `Messages::new_mail` 用同一个条件（`UNREAD_IN_INBOX`），攒的这一秒里在别处读过、移走的不再提醒。
- 三封以内逐封通知（标题是发件人，正文是主题），多了合成一条"N new messages"，正文是不重复的发件人。Linux 上
  正文是标记（通知服务支持 `body-markup` 时），所以转义 `&`、`<`、`>`：邮件里的 `<b>`、`<a href>` 照字面显示。
  标题按规范是纯文本，不转义。
- 设置"Notify about new mail"（Settings › General）默认开，关掉时攒够也不重读、不发。

**Linux**：⚠️ nsh 没有通知，用 GLib 的 `GNotification`，经 `gtk::Application` 发出（`category` 是
`email.arrived`）。没有沙箱时 GLib 走 `org.freedesktop.Notifications`，点击回来是应用的 `open-mail` action，带着
通知的 tag（`Target`：`new-mail/账户/邮件`、`new-mail/账户`，多个账户的合成通知是 `new-mail`）。它变成一个去处
（`Destination::Inbox { account, message }`），交给 `shell::show_at`：主窗口开着就转过去；没开就记下，打开主窗口，
新的邮箱读完文件夹后转过去。哪个是 INBOX 由邮箱按它读到的文件夹决定，与它默认打开的收件箱同一个。邮件可能刚到、
列表还没读到它：列表记着要选的那封，读到时选中（`list::Reveal`）；只有点击之后开始的读也没有它，才放弃、只打开
INBOX。这之前用户自己选了别的邮件，就以用户的为准。点击总是去账户的 Inbox，即使窗口正显示 All Inboxes：合并的
列表里旧一些的新邮件可能不在已加载的那一页。

在 Flatpak 里 GLib 改走 Notification portal，在 GNOME Shell 上改走 GNOME 自己的接口：点击不再回到进程里，而是由
portal（xdg-desktop-portal-gtk）或 GNOME Shell 对总线名 `net.yangkx.catmail` 调用
`org.freedesktop.Application.ActivateAction`（GLib 的 portal 后端不听 `ActionInvoked`）。应用是 `NON_UNIQUE`
（单实例靠 socket），GApplication 照样在对象路径上导出这个接口和 action，只是不占名字；所以 `shell/gtk.rs` 在
startup 时自己占用 app id（`DO_NOT_QUEUE`），不改变单实例的做法。另一个数据目录的实例先占了时，这个实例的
通知点击会到那一个，它找不到邮件、只打开自己的窗口。

**macOS**：还没有。系统通知（`UNUserNotificationCenter`）只能在 bundle 里用，在 bundle 外会让进程崩溃；磁盘映像里
是 bundle 了，接上之前 macOS 上没有新邮件通知。

## 托盘

GUI 的 `tray/` 在引擎打开时启动，有窗口时、常驻时都在。`tray/mod.rs` 数未读、在后台把图标画成像素；
`linux.rs`、`macos.rs` 把画好的图标和未读数交给 frontend 里各平台的托盘项。不依赖 UI 库的部分在
`catmail_frontend::tray`：菜单（`MENU`、`Command`）、提示文字、角标的文字和带角标的 SVG（`badged`）、按平台给出
图标的样子和尺寸（`icon`，图标的 SVG 在 `crates/catmail-frontend/assets/app-icon/`）、什么时候数、什么时候画、
画哪个（`TrayModel`）、Linux 的 StatusNotifierItem（`tray::linux`，ksni 本来就不依赖 UI 库）、macOS 的状态栏项
（`tray::macos`，AppKit 本身）。

- **未读数**：`MailClient::count_unread_in_inboxes` 把各账户 INBOX（Gmail：INBOX 标签）的 `unread_count` 加起来，
  一次读。侧栏要重读的事件（`Refresh::after(..).sidebar`：消息、文件夹、账户的变化，漏掉事件）都重数；数没变
  就不重画。托盘注册上了才开始数：没有托盘的桌面不读。
- **图标**（Linux）：应用图标（`app-icon/` 的亮色或暗色版本）右上角叠一个红色角标写着未读数，超过 99 写
  "99+"，没有未读就不画。拼成一个 SVG，用 resvg（nsh-core 已经带着，另开 `text` 画数字）在另一个线程上画成
  22、32、64 像素，由托盘挑合适的。提示文字是"N unread messages"。亮暗跟随 portal 的 `color-scheme`，即桌面的
  外观而不是应用的，常驻时也跟随。
  - 画也一次一个：后台的两次绘制可能先后颠倒，旧的数字盖掉新的之后就一直错着；画的途中又有变化，画完按当时的
    数字和外观再画一次。记着上次画的 SVG，同样的不再画，只更新数字。
  - 数字用 `sans-serif`：fontdb 从 fontconfig 的别名里取，可能是没装的字体（Ubuntu 上就是），这时依次换成常见的
    无衬线字体，再没有就用任意一个装了的。
- **点击图标**打开主窗口（macOS 上打开菜单）；**菜单**："Open catmail"、"New message"、"Sync now"、"Quit"。都转成
  `Command`，经 channel 回到主线程（`shell::chose`）：
  - 打开：`shell::show_at`，与点击通知相同。
  - 写邮件：主窗口开着就在它里面写（与 Ctrl N 相同，用当前账户）；关着就记下，打开主窗口，新的邮箱读完文件夹
    后再写。
  - 同步：`MailClient::sync_now`，不论主窗口开着与否，与 Sync 按钮相同（见[同步 / 后台同步](sync.md#后台同步与-idle)）。
  - Quit：`shell::quit`，与 Ctrl Q 相同。
- **Linux** 用 `ksni`（StatusNotifierItem，纯 D-Bus，不依赖 GTK 主循环），用 `async-io` 运行时，与依赖树里已有的
  zbus 同一个，不引入 tokio。KDE、装了 AppIndicator 扩展的 GNOME、waybar 等都显示它；没有 StatusNotifierWatcher
  （GNOME 没装扩展）就不显示，记一条日志，靠再次启动唤起。托盘给的 XDG 激活令牌（`ksni::current_xdg_activation_token`，
  KDE 可能晚到一点，等 1 ms 再取一次）随命令传给外壳，主窗口用它获得焦点。测试里不注册（`cfg(test)`）。
- **macOS** 直接用 AppKit（`objc2-app-kit`）：`NSStatusItem` 的按钮放图标，菜单是 `NSMenu`，菜单项的 target 是一个
  只转发命令的小类（`define_class!`，tag 是在 `MENU` 里的位置），`Item` 留着它（菜单项不 retain target）。都在主
  线程上。图标由前端画好交过来（`Template`：预乘 alpha 的 RGBA，AppKit 位图的格式）。
  - 图标是单色的模板图（`app-icon/catmail-menu-bar.svg`：信封和猫爪），系统按菜单栏的亮暗、强调色着色，
    不用跟外观重画，只画一次（18、36 像素，18 pt 的 1x、2x）。未读数是按钮的标题，写在图标右边，与 Linux 角标
    同样的文字；提示文字相同。
  - 同一个数也作 Dock 图标的角标（`NSDockTile::badgeLabel`，Mail.app 的做法）。
  - 没有用 `tray-icon`：它在 macOS 上也是这些 AppKit 调用，外加 `muda` 的菜单和它自己的事件循环约定；直接写只有
    一百多行，图标、标题、角标都由我们决定。
  - `define_class!`、位图的缓冲区要 `unsafe`，由 `catmail_frontend::tray` 对 `macos` 模块单独放行。

## 退出

退出由外壳一处决定（`shell::quit`；Ctrl Q、托盘的 Quit、macOS 应用菜单和 Dock 的 Quit、信号都调用它）：

- 记下退出原因，让每个写信窗口走关闭流程、存下最后的输入（`OpenComposers::close_all`）。全部关掉了就关掉所有窗口，
  应用随最后一个窗口结束（没有窗口时立即结束）；有一个关不了（地址要改、保存或附件失败）就取消退出，那个窗口
  留下，记 `quit called off`。
- 退出途中最后一个窗口关掉时，`after_last_window` 看到正在退出，不会进入常驻。
- 写信窗口只回答"关不关得了"，不知道退出：起初退出状态放在写信窗口模块里，由最后一个关掉的写信窗口让应用退出，
  退出的决定分在两处，以后加入口（托盘）或退出前的确认都得改两边。
- 主循环结束后记一行 `quitting reason=…`（`QuitReason`：`shortcut`、`tray`、`signal`、`last_window_closed`、
  `engine_failed`、`no_window`；没记原因的是平台自己的退出，记 `platform`），以前日志只是停住，分不清正常退出和
  进程被杀、崩溃。macOS 上 `applicationShouldTerminate` 先答取消，走同样的流程，存完再停主循环。⚠️ 注销时这样
  会被系统当成应用拒绝退出（ROADMAP）。
- SIGHUP、SIGINT、SIGTERM 走同一个 `shell::quit`，写信窗口先存好再退出：会话注销时系统发的是 SIGTERM，这样没保存
  的草稿也能留下。第二个信号不再等写信窗口，直接销毁窗口、结束主循环。Linux 上用 GLib 的 unix 信号源，直接在
  主循环上；macOS 上用 `ctrlc` 在它自己的线程上等，再回到主线程。主循环不是 tokio，用不了 `tokio::signal`。
- 不能指望最后一个 `MailClient` 句柄被 drop（视图、全局、等结果的任务都可能拿着）。主循环结束后外壳从
  `engine.rs` 取出引擎并调 `MailClient::shut_down`：不管谁还拿着句柄，引擎都关闭，记 `engine closed`；runtime 是
  自建的，进行中的工作随之停下，它们的调用得到 `Error::Closed`。然后在主线程上等 `MailClient::closed()`，这时主
  线程上已经没有别的事。

## 开机启动

启动时开不开窗口由外壳在工具包启动完时决定（`shell::launched`：GTK 的 `activate`，AppKit 的
`applicationDidFinishLaunching:`），规则是纯函数 `catmail_frontend::resident::at_launch`：启动要求不开窗口
（`catmail-gui --background`，或者是登录时的启动），开着"关闭窗口后保持运行"，引擎也没打开失败，就不开窗口，
直接常驻，同最后一个窗口关掉之后一样；否则照常开主窗口。关着"保持运行"时关窗就退出，从不开窗口的启动会什么也
留不下，所以照常开窗口。引擎打开失败要有窗口说明原因：启动时已知就开窗口；失败时没有窗口（不开窗口的启动，或引擎打开期间
窗口被关掉了）就开一个，关掉它应用就退出（`after_last_window`，记 `engine_failed`）。

"开机时启动"（Settings › General 的 "Open at login"）在系统那里，不进 `settings.json`：用户也能在系统设置里改，
所以每次打开设置时读系统的状态（`catmail_frontend::autostart`）。只在能用的地方显示这个开关（`available`）。

- **macOS**：`SMAppService.mainApp`（macOS 13 起），出现在系统设置 › 通用 › 登录项的"登录时打开"里，带应用的
  名字和图标。打开开关时 `register`，关掉时 `unregister`，在另一个线程上调（是对系统的阻塞调用，打开时系统还会
  通知用户加了登录项）；之后按系统的状态显示开关。系统答复之前开关不接受按动：结果说不准（可能要批准），
  两次改动同时在两个线程上做又可能先后颠倒（先 unregister 后 register，留下开着的登录项）。改动的线程比发起它的
  页面活得久（切换分类、关掉设置再开），所以 `autostart` 的 `set` 和 `state` 在全应用一次只做一个。页面打开时也在
  另一个线程上读状态，读到之前开关同样不接受按动：重开的页面等进行中的改动做完，读到的是改动之后的，视图线程
  不跟着等。没有用排队只做最新一次的办法，那要再加序号，为一个很少连按的
  开关不值得。状态是 `requiresApproval`（在系统设置里被关掉了）时打开
  系统设置的登录项页面。只在开关上调用 `register`，启动时不调：否则用户在系统设置里关掉的又会被打开。
  - 只在 bundle 里能用：`cargo run` 的二进制没有 bundle id，不显示开关。最低 macOS 12，12 上没有 `SMAppService`
    这个类，也不显示（直接用会 panic）。
  - 没有考虑旧的 `SMLoginItemSetEnabled`（要在 bundle 里另带一个辅助应用）和 `SMAppService.agent`（LaunchAgent
    的 plist 能直接带 `--background`，但它列在"允许在后台"而不是"登录时打开"里，用户找不到它是什么）。
- **怎么知道是登录时的启动**：macOS 不给参数，看启动时的 Apple Event（`autostart::launched_at_login`，在
  `applicationDidFinishLaunching:` 里，那时它还是当前的事件）：
  - 旧的登录项打开应用时，"open application" 带 `keyAEPropData` = `keyAELaunchedAsLogInItem`：是登录时的启动。
  - 没人打开应用时（从 shell 或由 launchd 直接启动），AppKit 给自己发一个空的 "open application"，发送方
    （`keySenderPIDAttr`）是自己的进程（实测，macOS 27）。有报告说 `SMAppService.mainApp` 的登录项由 launchd
    启动，没有启动事件；于是"没人打开、登录项开着"也算登录时的启动。从 shell 直接运行 bundle 里的二进制、登录项
    又开着时，因此也不开窗口，再启动一次就开。
  - Finder、Dock、`open` 打开的：不是。
  - 日志记一行 `launched opened=… at_login=…`。⚠️ 还没在真的登录上确认过哪一种。
- **Linux**（还没做）：写 XDG autostart 文件，`Exec` 带 `--background`。包是 Flatpak（见
  [打包](packaging.md#图形界面flatpak)），沙箱里写不了 `~/.config/autostart`，要用 Background portal 的
  `RequestBackground`（`autostart`、`commandline` 带 `--background`），由 portal 写出 `flatpak run` 的 autostart
  文件；包外运行时才自己写。
- **Windows**（随 Windows 的 GUI）：`HKCU\…\Run`。

`--background` 只管这一次启动：已经有实例在运行时，它照样把 `Activate` 交给那个实例，主窗口会出来。

## 与 CLI 的关系

GUI 常驻就一直持有数据目录锁，CLI 更常遇到 `AlreadyRunning`。CLI 打不开数据目录时说明"catmail 应用
关掉窗口后仍在运行，先退出它"。让 CLI 经唤起服务所在的端点使用正在运行的引擎，另开 issue。

## 实施顺序

1. ✅ 引擎属于应用；"关闭窗口后保持运行"（Settings › General）；主窗口可以反复关闭、重开（#106）。
2. ✅ 单实例与唤起：数据目录里的 socket、激活令牌（#106）。
3. ✅ 引擎：`resume()`、自己发现睡眠、`Event::NewMail`，各有假服务器测试（#106）。
4. 系统通知：✅ Linux（#82）；macOS。
5. 各平台托盘：✅ Linux（#107）；✅ macOS（#108）；Windows（#109，随 Windows 的 GUI）。
6. `--background` 与开机启动：✅ `--background`；✅ macOS；Linux；Windows。

这几步最初在 GPUI 版上做完，换到 nsh 时外壳重写，frontend 里的部分没动。
