# 退出时等引擎有上限，macOS 注销不再被取消

- 日期：2026-11-20
- 类型：修复

## 背景

- 主循环结束后 `pollster::block_on(client.closed())` 不限时：引擎里一次卡住的写入就让进程退不出去，会话结束时
  系统等不到它退出。GPUI 版最多等 GPUI 的 `SHUTDOWN_TIMEOUT`，换成 nsh 时丢了这个上限。
- macOS 上 `applicationShouldTerminate` 先答 `TerminateCancel`，写信窗口存完再停主循环。注销时系统把这当成应用
  拒绝退出，取消注销，过一会儿再问一次。

## 决策

- 上限定 2 秒：本机日志里平时关引擎不到 1 ms，2 秒足够宽，又短到不拖住注销。到时间记一条 warn 直接退出，
  数据库的样子同进程被杀一样，SQLite 打开时恢复。
- 等待放在单独的线程上用 `recv_timeout` 计时：主线程上的 `pollster` 没有定时器，到时间就把那个线程留下，
  进程随即结束。
- macOS 答 `TerminateLater`，写信窗口存完时答 YES。没选「答取消、自己停主循环、进程退出时不答复」：
  `TerminateLater` 期间 AppKit 在 `terminate:` 里跑嵌套循环，只有答复能让它出来，`stop:` 不起作用；
  答 NO 又会取消注销。答 YES 后 AppKit 发 `applicationWillTerminate:` 再自己 `exit`，`run` 不返回，所以收尾
  （MCP、引擎）抽成 `shell::finish`，两条路径都调它。日志是同步写的，AppKit 的 `exit` 不会丢日志。
- 有写信窗口关不了时答 NO：注销随之取消，那个窗口要用户处理，同以前 Ctrl Q 被取消一样。
- 已经在停主循环（`end` 先于 AppKit 来问）时答 `TerminateNow`：这时再答 `TerminateLater` 就没有人答复了。

## 方案

- `shell/mod.rs`：`finish()`（原 `run` 结尾的收尾）加上 `closes_within(closed, ENGINE_CLOSING)`；
  取消退出时调 `toolkit::quit_called_off()`。
- `shell/appkit.rs`：`Termination`（Running / Asked / Ending）记 AppKit 是否在等答复；`end()` 在 Asked 时答 YES，
  否则照旧 `stop:`；`quit_called_off()` 答 NO；新增 `applicationWillTerminate:` 调 `finish`。
- `shell/gtk.rs`：`quit_called_off()` 什么也不做，GTK 不问应用。
- 验证：`closes_within` 的两个单元测试（马上关的等到、永不关的 20 ms 后放弃）；在 macOS 上用单独的数据目录启动
  `catmail-gui`（`--background` 和开着窗口各一次），按 pid 用 `NSRunningApplication.terminate()` 发 Quit，进程都
  以 0 退出，日志有 `quitting`、`engine closed`（没答复的话进程会卡在嵌套循环里）。AppKit 没问的路径用 SIGTERM
  试过（`stop:` 后 `run` 返回再收尾），同样以 0 退出。没有实际注销测试，也没测写信窗口关不了时的 NO。
- 没做：让 macOS 上 AppKit 没问的退出也走 `terminate:`，收尾只剩 `applicationWillTerminate:` 一处，删掉 `stop:`
  加假事件的做法。它改的是 Ctrl+Q、托盘、信号等本来正常的路径，另开 issue。
- ROADMAP 和 background.md「退出」随之更新；ROADMAP 里 Linux 安装包一项的旧说法（AppImage 撤下、只发 CLI）改成
  已发 Flatpak，另列「上 Flathub」。
