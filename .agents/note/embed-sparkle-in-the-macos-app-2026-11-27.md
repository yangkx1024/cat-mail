# macOS 的 app 嵌入 Sparkle，GUI 接上更新器

- 日期：2026-11-27
- 类型：功能
- Issue：#402（自动更新的第一步）

## 背景

macOS 版的发布流程已经能签名、公证，但没有自动更新，用户只能手动下载新版。自动更新选 Sparkle 2，并且支持增量更新。整个方案在 #402 里，分几个 PR 做；这是第一个：把 framework 放进 bundle 并签好名，把更新源和公钥写进 `Info.plist`，GUI 启动时起更新器、应用菜单加"Check for Updates…"。发布时生成 appcast 和增量包是下一个 PR，在那之前还收不到更新。

## 决策

- **更新源用自己的域名** `https://catmail.yangkx.net/appcast.xml`，不用 GitHub 的地址：
  - 只有 `SUFeedURL` 写死在发出去的 app 里。appcast 里的下载地址每次发布都能换。
  - 账户出问题、仓库改名、要把安装包挪出 GitHub，都不会让已经装好的 app 断掉更新。
  - 现在由 Cloudflare 的 Redirect Rule 302 转发到 `releases/latest/download/appcast.xml`。用 302 不用 301，避免客户端缓存了跳转，以后换托管时旧跳转还生效。
- **运行时加载 Sparkle，GUI 不链接它**：开发构建、`cargo run` 不需要 framework；`check_binary` 只允许系统库的检查也不用改。
- **删掉 XPC 服务**：只有进沙盒的 app 用得到它们，Sparkle 文档也说不进沙盒时可以删。
- **只留 arm64**：和 app 本身"只出 Apple 芯片的包"一致，framework 小了大约一半。
- **由里往外签名、不用 `--deep`**，按 Sparkle 文档的顺序：`Autoupdate` → `Updater.app` → framework → app。
- **固定 Sparkle 版本并校验 sha256**：Sparkle 不发布校验和，用的是固定这个版本时从它的 GitHub release 下载到的那份。
- **更新器放在 AppKit 外壳里**（`catmail-gui` 的 `shell/appkit/updater.rs`），不放 `catmail-frontend`：它要在建菜单之前起、做菜单项的目标，离不开 AppKit，也没有可以单独测试的模型。
- **默认策略用 Sparkle 的**：第二次启动时问一次要不要自动检查，自动下载但不静默安装，设置留在 Sparkle 自己的 user defaults 里，暂不进 `settings.json`。
- **退出不另做**：Sparkle 安装时发退出的 Apple Event（`NSRunningApplication terminate`），和 Dock 的 Quit 走同一个 `applicationShouldTerminate:`，写信窗口先存好；有一个关不了时退出取消，Sparkle 的安装程序会等到应用之后退出时再装。
- **ad hoc 签名的包没有更新器**：hardened runtime 的库校验只让加载 Apple 或同一团队签名的库，ad hoc 签名没有团队，`loadAndReturnError` 失败（本地验证过：去掉 hardened runtime 就能加载）。不加 `disable-library-validation`，那会让任何库都能注入进来；要试更新用 `build_only` 的签名包。
- **`CATMAIL_UPDATE_FEED` 换更新源**，用 updater delegate 的 `feedURLStringForUpdater:`，不用 Sparkle 已经弃用的 user defaults 里的 `SUFeedURL`：只在设了变量的那次运行有效，不会留下来。
- **EdDSA 私钥不经过仓库和 CI 以外的地方**：在本机用 `generate_keys --account catmail` 生成，导出后放进 `release` environment 的 `SPARKLE_ED_PRIVATE_KEY`，另有离线备份。

## 方案

- `packaging/macos/build-dmg.sh`：
  - 下载 Sparkle 2.10.1，校验 sha256，解压到 `target/macos/tools/sparkle-<版本>`，下次直接用。
  - 用 `ditto --arch arm64` 拷进 `Contents/Frameworks/`（保留符号链接，拷的时候就只留 arm64），删掉 XPC 服务，Sparkle 的许可证拷进 `Resources/licenses/`。
  - 按上面的顺序签名。
- `packaging/macos/Info.plist`：加上 `SUFeedURL` 和 `SUPublicEDKey`。
- `crates/catmail-gui/src/shell/appkit/updater.rs`：bundle 里有 Sparkle、有更新源时，用 `NSBundle` 加载 framework，起 `SPUStandardUpdaterController`；否则记一行日志，不起。`CATMAIL_UPDATE_FEED` 设了时带一个只回答 `feedURLStringForUpdater:` 的 delegate。
- `shell/appkit.rs`：建菜单之前起更新器，更新器一直留到进程结束（菜单项不持有它的目标，Sparkle 也是这样用它的）；应用菜单在 About 下面加"Check for Updates…"。
- `docs/architecture/packaging.md`：bundle 结构、签名顺序，"自动更新"一条替换原来的"不自动更新"。`background.md` 的退出、`gui.md` 的菜单栏提到 Sparkle。
- 还没做：常驻（没有窗口）时，定时检查找到更新会直接弹出 Sparkle 的窗口，以后用通知或托盘提示（Sparkle 的 gentle reminders）。
