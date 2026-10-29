# 后台运行

[← 架构](../architecture.md)

> 状态：常驻与单实例唤起、睡眠唤醒、新邮件事件与系统通知、Linux 托盘已实现（#106、#82、#107）；
> Windows、macOS 托盘（#108 / #109）、开机启动尚未实现，这几个小节是设计。

GUI 关掉主窗口后继续同步、发信、提醒新邮件，并能从托盘、再次启动或通知回到窗口。

## 进程模型：GUI 进程常驻

关掉主窗口后 GUI 进程不退出，只释放窗口；引擎、托盘、通知都在这个进程里。

考虑过"守护进程持有引擎、GUI 经 IPC 连接它"，没有采用，按可维护性也不更好：

- `MailClient` 的六十多个 async 方法、事件流、文件交接都要过 IPC，两个进程的版本要配对。
- 守护进程省不掉 UI 循环：macOS 的状态栏图标要 NSApplication 主线程，它实际上还是一个 UI 进程。
- 常驻 GUI 的开销已经接近守护进程：GPUI Fast 能在运行时切到 headless（见下），Linux 上的托盘
  （StatusNotifierItem）和通知都走 D-Bus，不需要显示服务器的连接。

守护进程真正的好处是 CLI 能与 GUI 同时使用，这留给"引擎 IPC"（见[与 CLI 的关系](#与-cli-的关系)）。
常驻的部分集中在一个模块里，以后要拆成独立进程时整体搬出去。

## 两个状态

```
有窗口 ──最后一个窗口关闭，且开着"关闭窗口后保持运行"──▶ 常驻
  ▲                                                    │
  └──── 托盘 / 再次启动 / 点击通知 / Dock ───────────────┘
有窗口、常驻 ──Quit（Ctrl Q、托盘菜单、macOS 应用菜单）──▶ 退出
```

- 应用用 `QuitMode::Explicit`，何时退出由我们决定，不再在 `on_window_closed` 里 `cx.quit()`。
- 设置"关闭窗口后保持运行"（`settings.json`）默认开。关闭时与以前相同：最后一个窗口关掉就退出。
  默认开，是因为没有托盘的桌面（GNOME 未装 AppIndicator 扩展）也能从启动器再点一次回到窗口
  （见[单实例与唤起](#单实例与唤起)），不会"找不回来"。
- 写信、设置窗口也算窗口：关掉主窗口时还有写信窗口开着，就等它们都关掉才进入常驻。
- 进入常驻时 `cx.request_windowing(WindowingRequest::Headless)`，回到窗口前
  `request_windowing(Windowed(env))`：

  | 平台 | 常驻时 |
  |---|---|
  | Linux | 断开 Wayland / X11 连接；托盘和通知经 D-Bus |
  | Windows | 释放 GPU；消息循环照常，托盘图标需要它 |
  | macOS | 不切换，保持 Regular（Dock 图标），合 Mail.app 的习惯；只留状态栏图标的模式在 #108 决定 |

  被另一个进程唤起时，`env` 带上那个进程的 `XDG_ACTIVATION_TOKEN`，否则 Wayland 不让新窗口获得焦点。

## 引擎属于应用，不属于窗口

应用级的全局 `Resident`（`catmail-gui/src/resident.rs`）持有：

- 打开中、已打开（`MailClient`）或打开失败的引擎；主窗口从它取，写信窗口和设置从主窗口拿到。
- 主窗口（`MainWindow`：引擎打开前留空，失败时显示原因，之后是 `Mailbox`）和显示模式的切换。
- 唤起服务送来的请求（下一节）。
- 窗口最后在的显示环境（`display`）：点击通知时没有别的请求方，常驻时就切回这里。

不依赖窗口的事件订阅放在各自的全局里，引擎打开时启动：新邮件通知（`Notifier`，见[新邮件](#新邮件)）、
托盘（`Tray`，见[托盘](#托盘)）。

主窗口关闭时整个 `Mailbox` 被 drop：列表、读信区（Blitz 每个文档一个线程）、图片都随之释放。重新
打开时新建 `Mailbox`，`Resident` 只记住上次选中的 `Place`（`Mailbox` 释放时记下，新的
`Mailbox::reopen_at`），那里已经不在就回到第一个收件箱。

最后一个窗口关闭时怎么做是一个纯函数（`after_last_window`）：正在退出、没开"保持运行"、引擎打开失败
（没有东西可运行）时退出；否则 Linux、Windows 上切到 headless，macOS 上什么也不做。切换是异步的，
期间到来的唤起请求等切换结束再处理；切到窗口模式失败（例如请求方没有显示服务器）就留在 headless，
只记日志。GPUI 只在没有窗口时允许切换，正合这里的用法。`Mailbox::new` 不做启动同步（后台同步
打开时就完整同步一遍），重开窗口不会多出同步；打开时的 `report_refused_changes` 正好报告常驻期间被
拒绝的改动。

## 单实例与唤起

常驻之后，从启动器再点一次 cat-mail 应当唤起已有实例的窗口，而不是显示 "Couldn't open the mail
data"。macOS 由系统保证单实例、Dock 点击经 GPUI 的 `on_reopen`；Linux、Windows 上 GPUI 不提供
（Linux 的 `on_reopen` 从不触发），自己做：

- **端点是数据目录里的 Unix socket `gui.sock`**（`catmail-gui/src/instance.rs`），由持有数据目录锁的
  进程监听：目录只有本用户可访问，socket 本身也设成 0600。Windows 10 起也有 AF_UNIX，std 不支持，用 `uds_windows`（zbus 已经
  带进依赖树），三个平台一套代码；没有选 named pipe，那要自己设 DACL。
- **路径太长时放进运行时目录。** socket 地址最多约 100 字节（`sun_path`：Linux、Windows 108，macOS 104），
  数据目录的路径太长（例如很深的 `--data-dir`）时绑定会失败，于是改放在用户自己的运行时目录：Linux 上是
  `$XDG_RUNTIME_DIR`（经 `directories`；没有它就不放，绝不放进别人也能写的 `/tmp`），其他平台是每个用户的
  临时目录。文件名取数据目录规范路径的 FNV-1a 哈希：不用 Rust 的 `DefaultHasher`，它在不同版本间可能变，
  而升级后的新版本要找得到还在运行的旧版本。放哪里也按规范路径（`dunce::canonicalize`，Windows 上不带
  socket 路径用不了的 `\\?\` 前缀）的长度决定：否则一个启动用很长的真实路径（socket 进运行时目录）、
  另一个用指向它的短链接（找 `链接/gui.sock`），两者争同一把锁却找不到对方。
- **谁是实例由引擎的锁决定**，在 GPUI 启动之前，不会闪出窗口：
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
- 收到 `Activate`：常驻时用请求方的显示环境切回窗口模式，再打开主窗口，激活令牌在新窗口打开时用掉；
  已有主窗口就激活它。切换期间又来的请求记在 `Windowing::Switching` 里：切换成功，新窗口就是答复；
  失败（请求方的显示环境不可用）时改用这个请求的显示环境再试，它已经得到"好"的答复，不能丢掉。后一种情况 GPUI 不能用外来的令牌，它自己申请的令牌多半被 compositor 拒绝，
  窗口可能只被标成"需要注意"。
- 监听在一个专门的线程上（阻塞的 accept），请求经 channel 交给 GPUI 的前台任务。

## 系统睡眠与唤醒

`Engine::resume` 有两个来源：`MailClient::resume()`，`Resident` 在 GPUI 的 `on_system_wake` 里调用
（Linux 上 GPUI 听 logind 的 `PrepareForSleep`，常驻时也在）；引擎自己发现睡眠（见下）。常驻之后这是最
容易出问题的一处：

- Linux、macOS 上 `Instant` 和 tokio 的定时器不计睡眠时间，醒来后下一次完整同步最多还要再等 15 分钟；
  发件箱等重试的定时器同样晚到。
- 后台任务醒来后接着用 IDLE 那条连接，它多半已经断了，要等超时或 keepalive 才发现。

`Engine::resume` 只记下醒来的时刻（`woke`），哪些东西过时由各处用到时比较（`Engine::since_woke`）：
睡前做的事都记着自己从什么时候开始，早于这个时刻的不再算数。比"醒来就清空"更准：清空只管当下，
跨过这一刻、醒来后才做完的事会把过时的结果再记回去。

- **连接**：每条连接记着它建立的时刻（`Session::opened`）。要在一条连接上做事时（`on_session`，
  连接池和后台任务都经过它）换掉睡前建立的，不论它来自池里、是醒来时正被前台操作用着又放回去的，还是
  后台任务手上的。池里连接的存活时间（`IDLE_SESSION_TTL`）按 `Instant` 算，单靠它看不出睡过。
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
[同步 / 后台同步](sync.md#后台同步与-idle)）。醒来时网络可能还没连上，这一步失败就按平常的重试间隔
（从 30 秒起）再试；没有另外监听网络的变化。

### 引擎自己发现睡眠

`catmail watch` 和以后的前端不必各自监听系统唤醒：引擎在它存在期间另起一个任务（`sleep.rs`），
每 30 秒比较墙上时钟和 `Instant` 各走了多少。Linux、macOS 上前者计入睡眠、后者不计，
墙上时钟多走了 60 秒以上就算睡过，调用同一个 `Engine::resume`。

- 记下的醒来时刻是发现的那一刻，最多晚 30 秒（tokio 的定时器也不计睡眠，醒来后接着走完剩下的间隔），
  这中间开始的事重做一次：偏保守，不会出错。GUI 仍在 `on_system_wake` 里调用，醒来那一刻就响应。
  上次检查之后已经有人调用过 `resume`，任务就不再调用：再调用一次会把醒来时刻往后挪，丢掉这之间新开的
  连接、作废这之间的同步，再来一遍。
- 墙上时钟本身会跳：校时、用户往前调时间都会误报，代价是换一次连接、完整同步一次。60 秒的阈值远大于
  漂移和通常的校时；更短的睡眠发现不了，连接多半撑得过，定时器也只晚这么多。墙上时钟往回调不算。
- 不用计入睡眠的时钟（`CLOCK_BOOTTIME`、`mach_continuous_time`）：有了 `resume` 的比较，过时的同步时间
  和连接都已处理，与时钟计不计睡眠无关；tokio 的定时器和卡在死连接上的 IDLE 也要靠外部信号打断。
- Windows 上 `Instant` 可能计入睡眠，那样两边走得一样，发现不了；只在 GUI 收到系统唤醒时 `resume`。
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

GUI 的 `Notifier`（`catmail-gui/src/new_mail.rs`）是应用级的全局，引擎打开时订阅事件，常驻时也在：

- **先攒 1 秒再说**（`GATHER`）。一次同步按批提交、每批一个 `NewMail`，醒来后各账户又同时同步；
  不攒的话一阵新邮件会弹出好几条"N 封新邮件"。Linux 上 GPUI 不支持按 tag 替换通知，替换不了已经弹出的。
- **主窗口在前台时不说**（`Resident::main_is_active`，攒够时判断，这时就不再重读）。写信窗口在前台照发。
- **说之前重读**：`MailClient::unread_in_inbox` 用一条查询挑出各账户仍未读、仍在 INBOX（Gmail：仍带
  INBOX 标签）的，新的在前，与 `Messages::new_mail` 用同一个条件（`UNREAD_IN_INBOX`），攒的这一秒里在
  别处读过、移走的不再提醒。
- 三封以内逐封通知（标题是发件人，正文是主题），按从旧到新弹出，最新的在最上面；多了合成一条
  "N new messages"，正文是不重复的发件人。Linux 上正文是标记（通知服务支持 `body-markup` 时），
  GPUI 原样交给它，所以转义 `&`、`<`、`>`：邮件里的 `<b>`、`<a href>` 照字面显示，不变粗、不成链接。
  标题按规范是纯文本，不转义。
- 设置"Notify about new mail"（Settings › General）默认开，关掉时攒够也不重读、不发。

**点击**：通知的 tag 记着它说的是什么（`Target`：`new-mail/账户/邮件`、`new-mail/账户`，多个账户的合成
通知是 `new-mail`），GPUI 把它随点击送回。它变成一个去处（`Destination::Inbox { account, message }`），
交给 `Resident::show_at`：主窗口开着就交给它的 `Mailbox::go_to`；没开就记在 `Resident` 里，与关窗时记下的
位置是同一个（`reopen`），在窗口最后的显示环境上打开主窗口，新的 `Mailbox` 打开时转过去。哪个是 INBOX
由 `Mailbox` 按它读到的文件夹决定（`AccountFolders::with_role`），与它默认打开的收件箱同一个；文件夹还没
读完就等读完再转。邮件可能刚到、列表还没读到它：列表记着要选的那封，读到时选中
（`message_list::reveal`）；正在进行的那次读可能早于它到达，只有点击之后开始的读也没有它（不在第一页或
已经离开），才放弃、只打开 INBOX。这之前用户自己选了别的邮件，就以用户的为准。

**应用标识**：`set_app_identity("dev.CatMail.CatMail", "cat-mail")`，在第一个窗口之前；标识与数据、
配置目录的限定名相同。Windows 上它是 AppUserModelID，Linux 上 GPUI 只用名字（通知的 app name），
不带 desktop 文件 id 的提示；macOS 的通知要有 bundle，打包时（开机启动之后）再看。

已知的限制，都在 GPUI 的 Linux 实现里：

- 点击时通知服务给的激活令牌（`ActivationToken` 信号）没有传出来，Wayland 上由点击打开或激活的窗口
  可能只被标成"需要注意"，而不是来到前台。
- 不能收回通知（`dismiss_system_notification` 什么也不做）：已经读过的邮件的通知留在通知中心里，
  直到它自己过期。点击已经读过或移走的邮件的通知，仍打开 INBOX。
- 每条通知一个线程，等到它被点击或关闭（过期也算关闭）；通知中心一直留着的通知，线程也一直在。

## 托盘

GUI 的 `Tray`（`catmail-gui/src/tray/`）是应用级的全局，引擎打开时启动，有窗口时、常驻时都在。
与平台无关的部分在 `tray/mod.rs`，各平台只把图标、提示和菜单交给系统（`tray/linux.rs`；Windows、macOS
在 #108 / #109）：

- **未读数**：`MailClient::count_unread_in_inboxes` 把各账户 INBOX（Gmail：INBOX 标签）的 `unread_count`
  加起来，一次读。侧栏要重读的事件（`Refresh::after(..).sidebar`：消息、文件夹、账户的变化，漏掉事件）
  都重数，同一时刻只有一次在读，其间又有变化就读完再读一次（`Coalesce`，与侧栏相同）；数没变就不重画。
  托盘注册上了才开始数：没有托盘的桌面和还没有托盘实现的平台不读。
- **图标**：应用图标（`assets/app-icon/` 的亮色或暗色版本，按桌面的外观，与"System"主题读的是同一个，
  `appearance::system`）右上角叠一个红色角标写着
  未读数，超过 99 写"99+"，没有未读就不画。拼成一个 SVG，用 GPUI 的 `SvgRenderer` 在后台线程画成
  22、32、64 像素，由托盘挑合适的；不提交位图，也不另加 SVG 库。提示文字是"N unread messages"。
  桌面外观变化时重画；外观只能从窗口得知，常驻期间的变化要等下次开窗口。画也一次一个（`Coalesce`）：
  后台的两次绘制可能先后颠倒，旧的数字盖掉新的之后数字不再变，就一直错着；画的途中又有变化，画完按
  当时的数字和外观再画一次。
- **点击图标**打开主窗口；**菜单**（`MENU`，各平台按它建自己的菜单）："Open cat-mail"、"New message"、
  "Sync now"、"Quit"。都转成 `Command`，经 channel 回到 GPUI 线程（`Tray::choose`），由 `Resident` 处理；
  主窗口开着就在它的 `Mailbox` 上做（`Resident::in_main`），否则记下等它打开：
  - 打开：`Resident::show_at`，与点击通知相同。
  - 写邮件：主窗口开着就在它里面 `new_message`（与 Ctrl N 相同，用当前账户）；关着就记在 `Resident`
    里，打开主窗口时交给新的 `Mailbox`（`Resident::adopt`，与要去的位置一起），它读完文件夹后再写。
  - 同步：主窗口开着就走它的同步，进度和失败显示在窗口里；关着就在后台依次同步各账户，失败只记日志，
    回到窗口时显示为账户的状态。
  - Quit：`Resident::quit`，与 Ctrl Q 相同。
- **Linux** 用 `ksni`（StatusNotifierItem，纯 D-Bus，不依赖 GTK 主循环；`tray-icon` 在 Linux 上要
  GTK），用 `async-io` 运行时，与依赖树里已有的 zbus 同一个，不引入 tokio。KDE、装了 AppIndicator
  扩展的 GNOME、waybar 等都显示它；没有 StatusNotifierWatcher（GNOME 没装扩展）就不显示，记一条日志，
  靠再次启动唤起。托盘给的 XDG 激活令牌（`ksni::current_xdg_activation_token`，KDE 可能晚到一点，等 1 ms
  再取一次）随命令传给 `Resident`，Wayland 上切回窗口模式、新开的主窗口能获得焦点；已经开着的窗口
  仍只能请求激活（GPUI 不能把令牌交给已有窗口）。测试里不注册（`cfg(test)`）。
- Windows、macOS 用 `tray-icon`，在 GPUI 的主线程消息循环上（#108、#109）。

## 退出

退出由 `Resident` 一处决定（`Resident::quit`，绑在 Quit action 上，托盘的 Quit 也调用它）：

- 置 `quitting`，让每个写信窗口走关闭流程、存下最后的输入（`composer::close_all`）。它答"全部关掉了"
  就 `cx.quit()`；有一个关不了（地址要改、保存或附件失败）就答"没关完"，取消退出，那个窗口留下。
- 退出途中最后一个窗口关掉时，`after_last_window` 看到 `quitting`，不会先切到 headless。
- 写信窗口只回答"关不关得了"，不知道退出：起初退出状态放在写信窗口模块里，由最后一个关掉的写信窗口
  调 `cx.quit()`，`Resident` 还要去读它的标志，退出的决定分在两处，以后加入口（托盘）或退出前的确认
  都得改两边。

## 开机启动

`catmail-gui --background`：以 headless 启动，不开窗口，进入常驻。"开机时启动"的设置写 XDG
autostart 文件（Flatpak 用 Background portal）、Windows 的 `HKCU\…\Run`、macOS 的
`SMAppService.mainApp`。

## 与 CLI 的关系

GUI 常驻就一直持有数据目录锁，CLI 更常遇到 `AlreadyRunning`。CLI 打不开数据目录时说明"cat-mail 应用
关掉窗口后仍在运行，先退出它"。让 CLI 经唤起服务所在的端点使用正在运行的引擎，另开 issue。

## 实施顺序

1. ✅ `Resident` 持有引擎；`QuitMode::Explicit`；"关闭窗口后保持运行"（Settings › General）；常驻时
   headless；主窗口可以反复关闭、重开（#106）。
2. ✅ 单实例与唤起：数据目录里的 socket、激活令牌（#106）。
3. ✅ 引擎：`resume()`、`Event::NewMail`，各有假服务器测试；GUI 在系统唤醒时调用 `resume()`（#106）。
4. ✅ 系统通知（#82）。
5. 各平台托盘：✅ Linux（#107）；Windows（#108）、macOS（#109）。
6. `--background` 与开机启动（新 issue）。

实现前用一个小原型在 Wayland 上确认过：关掉窗口后切到 headless，定时器照常运行，D-Bus 通知照常发出，
再切回窗口模式能打开新窗口。只有一个空窗口的原型，常驻内存从 119 MB 降到 110 MB；真正的应用里
窗口占的更多（列表、读信区），实际数字待测。
