# macOS 上所有退出都经过 terminate:

- 日期：2026-11-20
- 类型：重构

## 背景

注销时不再取消（bound-the-quit-wait-and-answer-macos-terminate-later-2026-11-20）之后，macOS 上有两条退出路径：
AppKit 来问的（应用菜单和 Dock 的 Quit、注销）在 `applicationWillTerminate:` 里收尾，AppKit 自己结束进程；
应用自己的（Ctrl+Q、托盘、信号、最后一个窗口、引擎打开失败）用 `stop:` 加一个假事件结束主循环（照抄 nsh
`run_app`），`run` 返回后再收尾。收尾分在两处、两种上下文里，以后改 `finish` 要两边都照顾到（#366）。

## 决策

- `toolkit::end` 在 AppKit 没问时调 `app.terminate(None)`。这时 `Termination` 已是 `Ending`，
  `applicationShouldTerminate:` 答 `TerminateNow`，所以每次退出都在 `applicationWillTerminate:` 里收尾。
- 先用一个独立的 Swift 小程序对比了 `terminate:` 与 `stop:` 加假事件：
  - 什么都没开、窗口上开着 sheet（`NSOpenPanel` 的 `beginSheetModal`）时，两种都能退出；
  - 弹出菜单在跟踪、或应用级 `NSAlert.runModal` 开着时，主 dispatch queue 不跑，两种都要等它们关掉才开始退出。
  nsh 的 alert 和文件面板都是 sheet，所以改成 `terminate:` 不比原来差。
- `run` 在退出时不再返回，`main` 里 `run` 之后的代码不会执行。现在那里只有把返回码转成 `ExitCode`，macOS 上本来就
  总是 0；`shell::run` 和 `appkit::run` 的注释写明了这一点。`app.run()` 之后的代码留着，只在别的东西停了主循环时
  才走到。

## 方案

- `shell/appkit.rs`：`end` 去掉 `stop:` 和假事件，改调 `terminate:`；`Termination` 和委托方法的注释随之更新；
  不再用的 `NSEventType` 去掉。
- ROADMAP 里「停主循环抄了 nsh `run_app`」一句改掉（#248 列的照抄代码少了这一份）；background.md「退出」一节改成
  只有一处收尾，并记下菜单跟踪和应用级模态时主队列不跑。
- 验证：在 macOS 上用单独的数据目录启动 `catmail-gui`，按 pid 发 Quit（`--background` 和开着窗口）、SIGTERM
  （`--background` 和开着窗口）、连发两个 SIGTERM、关着「保持运行」时用辅助功能接口点关闭按钮关掉最后一个窗口，
  进程都以 0 退出，日志有 `quitting reason=…` 和 `engine closed`。临时在 `app.run()` 之后加一行日志跑 SIGTERM，
  那行没出现，确认 `run` 没返回、收尾在 `applicationWillTerminate:` 里。Ctrl+Q、托盘的 Quit 和引擎打开失败没在
  真机上触发，它们和信号走同一段代码（`quit` → `exit` → `end`）。
