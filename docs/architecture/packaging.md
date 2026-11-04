# 打包

> 状态：Linux 的 AppImage（x86_64、aarch64）、macOS 的磁盘映像（Apple 芯片）和两者 CLI 的 tar.gz 已实现；
> Windows 的打包是 M6 余下的部分。

## Linux：AppImage

GUI 发布为 AppImage：`catmail-<版本>-<架构>.AppImage`，一个文件，不用安装，带自动更新。CLI 不放进去：
AppImage 只有一个入口，另发 `catmail-<版本>-<架构>-linux.tar.gz`（只有 `catmail` 和许可证）。

`packaging/linux/build-appimage.sh` 做全部工作，本地和 CI 都调用它：release 构建 `catmail-gui`，组装
AppDir，检查依赖，再用 appimagetool 打包，输出到 `target/appimage/`。

```
AppDir/
  AppRun                          XDG_DATA_DIRS 前面加上 $APPDIR/usr/share，再 exec catmail-gui
  net.yangkx.catmail.desktop      顶层这份给 appimagetool 和集成工具，usr/share/applications 里还有一份
  net.yangkx.catmail.svg          .DirIcon 指向它
  usr/bin/catmail-gui             RUNPATH 是 $ORIGIN/../lib
  usr/lib/                        libxkbcommon-x11、libxcb-xkb（见下）
  usr/share/catmail/fonts/        Noto Sans SC Regular、Bold
  usr/share/metainfo/net.yangkx.catmail.appdata.xml
  usr/share/icons/hicolor/scalable/apps/net.yangkx.catmail.svg
  usr/share/licenses/catmail/     本项目和两套字体的许可证
```

### 在旧 glibc 上构建，而不是用 Nix

glibc 只向后兼容：程序能在哪些发行版上跑，取决于构建时的 glibc。CI 在 Ubuntu 22.04（glibc 2.35，
GitHub 上最老的 runner）上用 rustup 的工具链构建。脚本检查二进制要求的 glibc 符号版本不高于 2.35，
程序解释器是标准的 `/lib64/ld-linux-x86-64.so.2`（aarch64 是 `/lib/ld-linux-aarch64.so.1`），否则失败。

没有用 Nix 打包（nix-appimage 之类把整个闭包连同 Nix 的 glibc 一起放进去）：GPU 驱动只能来自宿主机，
宿主机的 Vulkan / GL 驱动链接的是宿主机的 glibc，和 Nix 的对不上，加载失败（nixGL 要解决的就是这个）。
dev shell 里构建的二进制解释器在 `/nix/store`，上面的检查会拒绝它。

### 哪些库自带

二进制直接链接的只有 fontconfig、libxkbcommon、libxkbcommon-x11、libxcb 和 glibc；Wayland、Vulkan、
GL 由 GPUI 在运行时 dlopen；SQLite 编进程序；TLS 是 rustls 加系统信任库；密钥环、托盘、通知走 zbus
（纯 Rust 的 D-Bus）。

- **用系统的**：glibc、libgcc_s、fontconfig（读系统的配置，版本不一致会出问题）、libxcb、libXau、
  libXdmcp、libxkbcommon（要和显示服务器一致），以及 dlopen 的 Wayland、Vulkan loader、GL（跟着驱动）。
  这和 AppImage 惯例的 excludelist 一致。
- **自带**：libxkbcommon-x11 和它的 libxcb-xkb。只有 X11 用到，只装了 Wayland 的系统上可能没有。
  它们的 RUNPATH 设成 `$ORIGIN`：RUNPATH 只作用于一个对象自己的依赖，libxkbcommon-x11 要靠自己的
  RUNPATH 才找得到同目录的 libxcb-xkb。

脚本检查二进制和自带库的每个 `NEEDED` 都在这两张表里，不在就失败。这样以后加的依赖如果引入了新的
系统库，打包时就会发现，而不是等到用户的机器上启动失败。

### 字体、图标、桌面文件

- CJK 字体按 [GUI](gui.md#字体linux) 的约定放在 `usr/share/catmail/fonts/`，AppRun 把 `$APPDIR/usr/share` 加到
  `XDG_DATA_DIRS` 前面（未设置时补上默认的 `/usr/local/share:/usr/share`）。文件与 flake 里 dev shell 取的
  相同（noto-cjk `Sans2.004`），脚本按 sha256 校验。
- 图标用固定亮色的 `catmail-light.svg`：图标加载器不理会 `catmail.svg` 里的媒体查询。
- 桌面文件、AppStream 元数据的 id 都是 `net.yangkx.catmail`，与 `APP_ID` 相同。每个窗口都设了这个 app id
  （`layout::open_window`：Wayland 在创建窗口时读 `WindowOptions::app_id`，X11 要另调 `set_app_id`
  设 `WM_CLASS`），桌面文件的 `StartupWMClass` 也是它。任务栏、Alt-Tab 靠它找到窗口的图标和名字。

### 自动更新

appimagetool 把更新信息 `gh-releases-zsync|yangkx1024|cat-mail|latest|catmail-*-<架构>.AppImage.zsync`
写进 AppImage，同时生成 `.zsync`。AppImageUpdate、Gear Lever 等按它去 GitHub 最新的 release 里找对应
架构的 `.zsync`，只下载变了的块。所以 release 不能标成 prerelease（`latest` 不含 prerelease），每次都
要上传 `.zsync`。应用自己不检查更新。

### 固定的工具版本

appimagetool `1.9.1` 和 type2 runtime `20251108` 都固定到具体 release，并校验 sha256。两者的
`continuous` 标签会移动；不给 `--runtime-file` 时，appimagetool 在打包时下载 `continuous` 的 runtime。
新的 type2 runtime 是静态链接的，宿主机不需要 libfuse2（需要 FUSE；没有时可以
`--appimage-extract-and-run`）。appimagetool 本身也是 AppImage，CI 里没有 FUSE，用
`APPIMAGE_EXTRACT_AND_RUN=1` 运行。

二进制全部 strip（`[profile.release] strip = true`），panic 的回溯只剩地址，消息和位置仍会记进日志。

### 与开机启动的关系

AppImage 每次运行都挂载到不同的临时目录（`$APPDIR`），文件本身的路径在 `$APPIMAGE`。实现开机启动时，
autostart 文件的 `Exec` 要写 `$APPIMAGE`，不能写 `/proc/self/exe` 或 `$APPDIR` 下的路径（见
[后台运行](background.md#开机启动)）。

## macOS：磁盘映像

GUI 发布为 `catmail-<版本>-aarch64.dmg`，里面是 `catmail.app` 和一个指向 `/Applications` 的链接，拖过去
就装好了。CLI 另发 `catmail-<版本>-aarch64-macos.tar.gz`（`catmail` 和许可证）。只出 Apple 芯片的包：
Intel Mac 越来越少，universal 二进制要把包撑大一倍。

`packaging/macos/build-dmg.sh` 做全部工作，本地和 CI 都调用它：release 构建两个二进制，检查，组装
bundle，签名、公证，打成磁盘映像和 tarball，输出到 `target/macos/`。要 Apple 芯片的 Mac 和 Xcode
（GPUI 的 Metal 着色器在构建时编译）。

```
catmail.app/Contents/
  Info.plist                      由 packaging/macos/Info.plist 填入版本号
  MacOS/catmail-gui
  Resources/catmail.icns
  Resources/licenses/             本项目和 Noto Sans 的许可证
```

- **最低 macOS 12。** gpui-fast-apple 按 macOS 12 编译 Metal 着色器，脚本用同一个
  `MACOSX_DEPLOYMENT_TARGET` 链接，`LSMinimumSystemVersion` 也是它。脚本检查两个二进制只含 arm64、
  `minos` 是 12.0，并且只链接 `/System/Library` 和 `/usr/lib` 下的库：从 Homebrew 或 Nix 链进来的库
  在用户的 Mac 上没有。
- **为什么一定要 bundle。** `NSBundle` 没有 bundle id 时，GPUI 关掉系统通知（`UNUserNotificationCenter`
  在 bundle 外会让进程崩溃），所以 `cargo run` 起的 GUI 没有新邮件通知；单实例也要靠 LaunchServices
  按 bundle 保证（见[后台运行](background.md#单实例与唤起)）。bundle id 是 `net.yangkx.catmail`，与 `APP_ID`
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
- 然后从这个标签构建：`ubuntu-22.04` 和 `ubuntu-22.04-arm` 上各一份 AppImage、`.zsync` 和 CLI tar.gz，
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
  macOS 的包照样签名、公证。在 NixOS 上本地打不了正式包（见上），要一个能分发的包就用它。

本地打包：Linux 要在 glibc 不高于 2.35 的系统上（或这样的容器里）运行脚本，需要 curl、patchelf、binutils
和上面那些库的开发包；macOS 要 Apple 芯片的 Mac 和 Xcode。
