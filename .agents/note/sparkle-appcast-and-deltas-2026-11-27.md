# 发布时生成 Sparkle 的更新包、增量包和 appcast

- 日期：2026-11-27
- 类型：功能
- Issue：#402（第二步，第一步是 #403）

## 背景

#403 把 Sparkle 放进了 app，GUI 也起了更新器，但发布还不出 appcast，装好的 app 检查更新时拿到 404。这一步
让发布带上 Sparkle 要的三样东西：钉好票据的 app 打成的 zip、从旧版本到新版本的增量包、列出它们的
`appcast.xml`，都签好 EdDSA 签名，上传到 release。

## 决策

- **用 Sparkle 自带的 `generate_appcast`**，不手写 appcast 模板：它算增量包、签名，从 app 读最低 macOS
  和架构，增量包要的属性（`deltaFrom`、`deltaFromSparkleExecutableSize` 等）随 Sparkle 版本变，手写容易漏。
- **只留这次版本的一条**（`--maximum-versions 1`）：appcast 每次从头生成，只有一个下载地址前缀，旧版本的
  条目会指到这次 release 下面，是错的。Sparkle 只看最新的那条，留旧条目没有用。本地试过：这样仍然给每个
  旧版本算增量包。
- **增量包从 release 里发布的旧 zip 算**，不在本地重建旧版本：两边都是钉了票据的 app，增量包打出来的 app
  和完整包的一样，离线也能打开。CI 按版本号挑最近三个更旧、带 zip 的 release；三这个数只在 workflow
  里，脚本按给了几个旧 zip 就算几个。
- **只有公证的构建出更新**（`CATMAIL_UPDATE_KEY` 要求 `CATMAIL_NOTARY_KEY`）：Sparkle 装更新时不经过
  Gatekeeper，没公证的 app 不能发出去。PR 和 ad hoc 的包不出。
- **脚本自己检查 `generate_appcast` 的结果**：私钥和 app 的 `SUPublicEDKey` 对不上时它只警告、把签名
  留空。脚本数 enclosure 和签名，再确认 appcast 里只有这个版本的一条（旧 zip 里混进更新的版本时，
  `--maximum-versions 1` 留下的会是它），不对就失败。本地用对不上的密钥试过，确实只警告。
- **少了增量包只警告，不失败**：最初要求每个旧版本都有一个增量包，codex 在 #404 的 review 里指出，
  增量包超过完整包的 7/8 时 Sparkle 2.10.1 故意删掉它（`Appcast.swift` 的 `markDeltaAsIgnored`，不打印
  任何东西），这样要求会让一次正常的发布失败，重跑也一样。我们的包几乎全是一个开了 thin LTO 的 Rust
  可执行文件，增量包大到这个程度并不意外。本地用加了 200 MB 随机数据的测试版本复现过。少了增量包的版本
  下载完整包，照样能更新，所以改成逐个旧版本检查、缺的打一行警告。
- **下载地址前缀由 CI 给**（`CATMAIL_UPDATE_URL`），不写死在脚本里：脚本不用知道仓库在哪，本地试升级时
  也能指向别处。
- **release notes 生成一次，两处都用**：`version` job 推完标签后调 generate-notes，`macos` job 嵌进 appcast
  （Markdown，Sparkle 的更新窗口显示），`publish` job 用作 release 的说明，原来的 `--generate-notes` 去掉。
  放在 `version` job：`macos` job 生成 appcast 时就要用，`publish` 在它之后才跑；generate-notes 要写权限，
  而 `macos` job 拿着签名的 secrets，不给它写权限。
  没用 `releaseNotesLink` 指向 release 页面：Sparkle 会把整个 GitHub 页面塞进更新窗口。
- **release 标成 latest**（`--latest`）：更新源经 `releases/latest/download/appcast.xml` 转发，draft、
  prerelease 不算 latest。发布后读一遍 `catmail.yangkx.net/appcast.xml`，确认里面是这次的版本。

## 方案

- `packaging/macos/build-dmg.sh`：设了 `CATMAIL_UPDATE_KEY` 时，在磁盘映像检查完之后打 zip、拷入
  `CATMAIL_DELTA_FROM` 里的旧 zip（跳过同版本的）和 `CATMAIL_RELEASE_NOTES`，跑 `generate_appcast`，检查
  签名和增量包个数，删掉拷进来的旧 zip，输出在 `target/macos/updates/`。新加 `count` 帮手：在
  `pipefail` 下 `grep` 找不到时不让脚本静默退出（本地先踩到过）。空数组用 `${a[@]+"${a[@]}"}`，macOS 的
  bash 3.2 在 `set -u` 下展开空数组会报错。
- `.github/workflows/release.yml`：`version` job 出 `notes`；`macos` job 在签名的运行里写私钥文件、下载旧
  版本的 zip、写 release notes 文件，收集 `target/macos/updates/*`；`publish` job 用 `--notes-file`、
  `--latest`，最后检查更新源。
- `docs/architecture/packaging.md`：自动更新、CI 的 secrets 表、发布章节。
- 本地验证：用临时生成的 ed25519 测试密钥和三个 ad hoc 的测试版本，单独跑脚本里的这一段（有旧版本和
  notes、没有旧版本、密钥对不上），以及用假的 `gh` 跑挑旧版本的循环。真正的签名、公证路径由 `build_only`
  验证。
- 还没做：端到端升级（A→B 完整包、B→C 增量包）要等有两个带 Sparkle 的 release，或者用 `build_only` 的包
  配 `CATMAIL_UPDATE_FEED` 在本地试；常驻模式的 gentle reminders。
