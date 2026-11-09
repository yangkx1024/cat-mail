# 打包

> 状态：macOS 的磁盘映像（Apple 芯片）、Linux 的 Flatpak 包和 Linux、macOS 的 CLI tar.gz 已实现（Flatpak 还没上
> Flathub）；Windows 的打包是 M6 余下的部分。

## Linux

### CLI

`catmail-<版本>-<架构>-linux.tar.gz`（x86_64、aarch64），只有 `catmail` 和许可证。CI 在 Ubuntu 22.04（glibc 2.35，
GitHub 上最老的 runner）上用 rustup 的工具链构建：glibc 只向后兼容，程序能在哪些发行版上跑取决于构建时的 glibc。
CLI 不链接图形库：SQLite 编进程序，TLS 是 rustls 加系统信任库，密钥环走 zbus（纯 Rust 的 D-Bus）。

### 图形界面：Flatpak

GUI 发布为 `catmail-<版本>-<架构>.flatpak`（x86_64、aarch64），一个单文件 bundle，用 `flatpak install --user`
安装；它记着 Flathub 的地址，缺的 runtime 从那里装。运行时是 GNOME runtime（`org.gnome.Platform`），GTK 4、
WebKitGTK 6.0 和 GStreamer 都来自它，包里只有 `catmail-gui` 和 `catmail` 两个程序（bundle 约 17 MB）。

**为什么不是 AppImage。** GPUI 版发布过 AppImage（只要带几个 X11 库）。换成 nsh 之后 GUI 链接 WebKitGTK，
带上它要过三关，每一关都只有 hack：

- WebKit 的辅助进程（WebKitWebProcess、WebKitNetworkProcess）的路径编译在库里（`PKGLIBEXECDIR`），
  `WEBKIT_EXEC_PATH` 只有开发者构建才认。只能改 `.so` 里的字符串，再让 AppRun 先 `cd` 进挂载目录。
- 6.0 的网页进程一定跑在 bwrap 沙箱里，沙箱只挂 `/usr`、`/lib*`、`PKGLIBEXECDIR` 和 `LD_LIBRARY_PATH`，看不到
  AppImage 的挂载点；还要宿主机有 bwrap、允许非特权 user namespace（Ubuntu 24.04 默认用 AppArmor 限制）。退路
  `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS` 意味着不开沙箱渲染陌生人的 HTML。
- 包里的 WebKit 冻结在构建时，每个 WebKitGTK 安全公告都要我们重发一版；GNOME runtime 由 Flathub 更新。

另外 GTK 4.14 和 WebKitGTK 6.0 要 Ubuntu 24.04 起，基线升到 glibc 2.39，包也会超过 150 MB。Flatpak 里
WebKit 的沙箱照常工作：网页进程由 `flatpak-spawn --sandbox` 起在子沙箱里。

**权限**（`net.yangkx.catmail.yml` 的 `finish-args`）：网络、Wayland（X11 兜底）、`/dev/dri`，D-Bus 上只多两个：

- `org.freedesktop.secrets`：`keyring` 直接说 Secret Service（zbus），不走 portal。
- `org.kde.StatusNotifierWatcher`：托盘。规范要求托盘项占用 `org.kde.StatusNotifierItem-<pid>-<id>`，沙箱里的
  pid 在每个应用里都是同一个小数字，会撞名（占用它还要通配的 `--own-name`），所以在 Flatpak 里（有
  `/.flatpak-info`）ksni 不占名字，用连接的唯一名注册，watcher 都接受。

其余都走 portal，不用额外权限：

- 通知：GLib 在沙箱里自动用 Notification portal。点击由 portal 对 app id 的总线名调用 `ActivateAction`，应用
  自己占用这个名字（Flatpak 默认允许占用自己的 id），见[后台运行](background.md#系统通知)。
- 打开链接和附件（`GtkUriLauncher`、`GtkFileLauncher`）、保存附件（`GtkFileDialog`）。
- 开机启动要用 Background portal（见[后台运行](background.md#开机启动)，还没实现）。

**数据目录**在 `~/.var/app/net.yangkx.catmail/data/catmail`，与包外的 CLI（`~/.local/share/catmail`）不是同一个，
所以 CLI 也打进包里：`flatpak run --command=catmail net.yangkx.catmail`。

**构建**：`packaging/linux/build-flatpak.sh`，本地和 CI 都用它。构建没有网络：脚本先 `cargo vendor`
（crates.io 和 git 依赖）到 `target/flatpak/vendor`，生成指向它的 cargo 配置，manifest 把两者作为 source 带进去，
用 SDK 扩展 `rust-stable` 的工具链编译。上 Flathub 时要换成 `flatpak-cargo-generator` 生成的、逐个带校验和的
sources，Flathub 不收本地目录。本地运行 `packaging/linux/build-flatpak.sh` 构建、为当前用户安装并打出 bundle
（`--no-install` 只打 bundle）；要 flatpak-builder（或 Flathub 的 `org.flatpak.Builder`）和 cargo。和 AppImage
不同，NixOS 上也能打出能分发的包：编译在 SDK 里进行，不碰 Nix 的工具链。

留下的约定：

- 桌面文件、AppStream 元数据（`packaging/linux/`）的 id 都是 `net.yangkx.catmail`，与 `APP_ID` 和 Flatpak 的
  app id 相同。每个窗口都设了这个 app id（Wayland 的 app id、X11 的 `WM_CLASS`），桌面文件的 `StartupWMClass`
  也是它。任务栏、Alt-Tab 靠它找到窗口的图标和名字。
- 图标用固定亮色的 `catmail-light.svg`：图标加载器不理会 `catmail.svg` 里的媒体查询。
- 不用 Nix 打包（把整个闭包连同 Nix 的 glibc 一起放进去）：GL / Vulkan 驱动只能来自宿主机，链接的是宿主机的
  glibc，和 Nix 的对不上（nixGL 要解决的就是这个）。

## macOS：磁盘映像

GUI 发布为 `catmail-<版本>-aarch64.dmg`，里面是 `catmail.app` 和一个指向 `/Applications` 的链接，拖过去
就装好了。CLI 另发 `catmail-<版本>-aarch64-macos.tar.gz`（`catmail` 和许可证）。只出 Apple 芯片的包：
Intel Mac 越来越少，universal 二进制要把包撑大一倍。

`packaging/macos/build-dmg.sh` 做全部工作，本地和 CI 都调用它：release 构建两个二进制，检查，组装
bundle，签名、公证，打成磁盘映像和 tarball，输出到 `target/macos/`。要 Apple 芯片的 Mac 和 Xcode。
GUI 是 nsh 的 AppKit 后端，正文用系统的 WKWebView，bundle 里不带别的库和字体。

```
catmail.app/Contents/
  Info.plist                      由 packaging/macos/Info.plist 填入版本号
  MacOS/catmail-gui
  Resources/catmail.icns
  Resources/licenses/             本项目的许可证
```

- **最低 macOS 12**（沿用 GPUI 版定下的）：脚本按它设 `MACOSX_DEPLOYMENT_TARGET` 链接，`LSMinimumSystemVersion`
  也是它。脚本检查两个二进制只含 arm64、
  `minos` 是 12.0，并且只链接 `/System/Library` 和 `/usr/lib` 下的库：从 Homebrew 或 Nix 链进来的库
  在用户的 Mac 上没有。
- **为什么一定要 bundle。** 没有 bundle 时菜单栏显示可执行文件名、Dock 没有图标；系统通知
  （`UNUserNotificationCenter`）在 bundle 外会让进程崩溃，macOS 的新邮件通知因此只能在 bundle 里做（还没做）；
  LaunchServices 也按 bundle 把再次打开交给已在运行的那个（见[后台运行](background.md#单实例与唤起)）。bundle id 是 `net.yangkx.catmail`，与 `APP_ID`
  和数据目录的限定名相同。
- **图标**由 `catmail-light.svg` 渲染（resvg，从 crates.io 装固定版本）。源文件的底板占 512 中的 480，
  直接用会比系统的图标大一圈：脚本改它的 `viewBox`，让底板落在 Apple 图标网格的 824/1024 上，四周留出
  系统图标都有的边距，再用 `iconutil` 生成 16 到 1024 的 `.icns`。
- **签名**：Developer ID Application 证书，开 hardened runtime（公证的要求），带安全时间戳。不需要任何
  entitlement：没有 JIT、不进沙盒，钥匙串用的是不要 entitlement 的 generic password（`keyring`）。钥匙串
  条目只信任创建它的程序的签名要求（团队 ID 加 bundle id），之后同一张证书签的新版本直接能读；未签名的
  开发构建、CLI 读 GUI 存的密码时，系统会问一次是否允许。CLI 的签名标识是 `net.yangkx.catmail.cli`。
- **公证**两轮，用 App Store Connect API key（`notarytool`，以 Apple 的结论为准，不是退出码；不通过时
  打印公证日志）。第一轮把 app 和 CLI 打成一个 zip 提交，通过后把票据钉在 app 上，这样从磁盘映像拷出来的
  app 离线也能打开；单个可执行文件钉不了票据，CLI 由 Gatekeeper 联网查。第二轮提交签好名的磁盘映像，
  钉上票据，最后用 `spctl` 按 Gatekeeper 对下载文件的规则检查磁盘映像和 app。
- 没有签名身份时一律 ad hoc 签名、不公证：只能在构建的那台 Mac 上运行，PR 用它检查打包能跑通。
- 不自动更新。Sparkle 要另外维护 appcast 和签名密钥，以后再做。

### CI 里的签名

身份和公证的 key 是 `release` environment 的 secrets，只有手动运行（发布和 `build_only`）用它：

| Secret | 内容 |
|---|---|
| `MACOS_CERTIFICATE`、`MACOS_CERTIFICATE_PASSWORD` | Developer ID Application 证书和私钥导出的 `.p12`（base64）及其密码 |
| `APPLE_TEAM_ID` | 团队 ID，用来挑出身份、核对签名 |
| `APPLE_API_KEY`、`APPLE_API_KEY_ID`、`APPLE_API_ISSUER_ID` | App Store Connect 的 API key（`.p8` 的内容）、key ID、issuer ID，角色 Developer |

workflow 建一个临时钥匙串，导入 `.p12` 和它链到的中间证书 Developer ID Certification Authority G2
（从 Apple 下载，校验 sha256），再把身份的 SHA-1、团队 ID 和 key 的路径交给脚本
（`CATMAIL_SIGN_IDENTITY`、`CATMAIL_TEAM_ID`、`CATMAIL_NOTARY_KEY`、`CATMAIL_NOTARY_KEY_ID`、
`CATMAIL_NOTARY_ISSUER`）。本地签名、公证也是设这几个变量。

## 发布

`.github/workflows/release.yml`：

- 在 main 上手动运行（`gh workflow run release.yml`，可加 `-f version=0.2.0`）就是发布。`version` job
  定版本：不填就是当前版本的下一个 patch；填了就用它（`X.Y.Z`，可带 `v`），不能比当前旧，标签不能已存在；
  填当前版本则不改版本，直接发布（第一次发布 0.1.0 用这个）。它改 `[workspace.package]` 的 `version`
  （所有 crate 都继承它），`cargo update --workspace` 只更新 `Cargo.lock` 里的工作区 crate，提交
  "Release v<版本>"，打附注标签，用 `git push --atomic` 把提交和标签一起推上去：运行期间 main 有新提交
  时两者都不推，重新运行即可。
- 然后从这个标签构建：`ubuntu-22.04` 和 `ubuntu-22.04-arm` 上各一份 CLI tar.gz，`ubuntu-24.04` 和
  `ubuntu-24.04-arm` 上各一份 Flatpak bundle（先关掉 AppArmor 对非特权 user namespace 的限制，flatpak-builder
  的 bwrap 要用），
  `macos-15` 上一份签名、公证过的磁盘映像和 CLI tar.gz，最后创建 GitHub release（附 `SHA256SUMS`）。
  任何一个平台失败都不发布。
- 全在一次运行里：用 GITHUB_TOKEN 推的标签不会触发别的 workflow，所以不能靠"推标签触发构建"。也就没有
  按标签触发的入口，手动推的标签不会发布。
- 先打标签再构建，包是从发布的那个提交构建的，日志里的 commit 就是它，不带 `-dirty`。代价是构建失败时
  标签已经在了而没有 release：偶发的失败重跑失败的 job（版本 job 的输出会保留），代码的问题就发下一个版本。
- 推到 main 依赖 main 没有禁止 Actions 推送的保护规则；以后开启分支保护时要允许 `github-actions[bot]`。
- 改动 `packaging/` 或这个 workflow 的 PR 也构建（用当前版本，不打标签），只作为 artifact 上传，用来在
  合并前检查打包。PR 拿不到签名的 secrets，macOS 的包是 ad hoc 签名、没有公证的。
- 手动运行时勾上 `build_only`（`gh workflow run release.yml --ref <分支> -f build_only=true`）只构建：
  任何分支都行，用当前版本，不改版本、不打标签、不发布，包作为 artifact 上传。和 PR 走同一条路径；
  同时填了 `version` 会报错。它有自己的 concurrency 组，不会排在发布后面，也不会顶掉排队中的发布。
  macOS 的包照样签名、公证。本地没有签名身份时，要一个能分发的包就用它。

本地打包：macOS 要 Apple 芯片的 Mac 和 Xcode。
