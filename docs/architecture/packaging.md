# 打包

> 状态：Linux 的 AppImage（x86_64、aarch64）和 CLI 的 tar.gz 已实现；Windows、macOS 的打包是 M6 余下的部分。

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

### 发布

`.github/workflows/release.yml`：

- 在 main 上手动运行（`gh workflow run release.yml`，可加 `-f version=0.2.0`）就是发布。`version` job
  定版本：不填就是当前版本的下一个 patch；填了就用它（`X.Y.Z`，可带 `v`），不能比当前旧，标签不能已存在；
  填当前版本则不改版本，直接发布（第一次发布 0.1.0 用这个）。它改 `[workspace.package]` 的 `version`
  （所有 crate 都继承它），`cargo update --workspace` 只更新 `Cargo.lock` 里的工作区 crate，提交
  "Release v<版本>"，打附注标签，用 `git push --atomic` 把提交和标签一起推上去：运行期间 main 有新提交
  时两者都不推，重新运行即可。
- 然后在 `ubuntu-22.04` 和 `ubuntu-22.04-arm` 上从这个标签各构建一份 AppImage、`.zsync` 和 CLI tar.gz，
  最后创建 GitHub release（附 `SHA256SUMS`）。
- 全在一次运行里：用 GITHUB_TOKEN 推的标签不会触发别的 workflow，所以不能靠"推标签触发构建"。也就没有
  按标签触发的入口，手动推的标签不会发布。
- 先打标签再构建，包是从发布的那个提交构建的，日志里的 commit 就是它，不带 `-dirty`。代价是构建失败时
  标签已经在了而没有 release：偶发的失败重跑失败的 job（版本 job 的输出会保留），代码的问题就发下一个版本。
- 推到 main 依赖 main 没有禁止 Actions 推送的保护规则；以后开启分支保护时要允许 `github-actions[bot]`。
- 改动 `packaging/` 或这个 workflow 的 PR 也构建（用当前版本，不打标签），只作为 artifact 上传，用来在
  合并前检查打包。

本地打包也要在 glibc 不高于 2.35 的系统上（或这样的容器里）运行脚本，需要 curl、patchelf、binutils
和上面那些库的开发包。

### 与开机启动的关系

AppImage 每次运行都挂载到不同的临时目录（`$APPDIR`），文件本身的路径在 `$APPIMAGE`。实现开机启动时，
autostart 文件的 `Exec` 要写 `$APPIMAGE`，不能写 `/proc/self/exe` 或 `$APPDIR` 下的路径（见
[后台运行](background.md#开机启动)）。
