# macOS 的 app 嵌入 Sparkle，Info.plist 写上更新源和公钥

- 日期：2026-11-27
- 类型：功能
- Issue：#402（自动更新的第一步）

## 背景

macOS 版的发布流程已经能签名、公证，但没有自动更新，用户只能手动下载新版。自动更新选 Sparkle 2，并且支持增量更新。整个方案在 #402 里，分几个 PR 做；这是第一个，只改打包：把 framework 放进 bundle 并签好名，把更新源和公钥写进 `Info.plist`。GUI 接上更新器、发布时生成 appcast，是后面的 PR。

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
- **EdDSA 私钥不经过仓库和 CI 以外的地方**：在本机用 `generate_keys --account catmail` 生成，导出后放进 `release` environment 的 `SPARKLE_ED_PRIVATE_KEY`，另有离线备份。

## 方案

- `packaging/macos/build-dmg.sh`：
  - 下载 Sparkle 2.10.1，校验 sha256，解压到 `target/macos/tools/sparkle-<版本>`，下次直接用。
  - 用 `ditto` 拷进 `Contents/Frameworks/`（保留符号链接），删掉 XPC 服务，三个可执行文件用 `lipo -thin arm64` 只留 arm64，Sparkle 的许可证拷进 `Resources/licenses/`。
  - 按上面的顺序签名。
- `packaging/macos/Info.plist`：加上 `SUFeedURL` 和 `SUPublicEDKey`。
- `docs/architecture/packaging.md`：bundle 结构、签名顺序，"自动更新"一条替换原来的"不自动更新"。
