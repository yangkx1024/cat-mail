# macOS 版打包为签名、公证的磁盘映像

- 日期：2026-11-04
- 类型：功能

## 背景

M6 只做了 Linux 的 AppImage，macOS 上只能从源码 `cargo run`。不在 bundle 里运行时 GPUI 关掉了系统通知，
单实例也靠 LaunchServices 按 bundle 保证，所以 macOS 版先要有 `.app`；有 Apple 开发者账号，可以签名、公证，
下载后 Gatekeeper 不拦。

## 决策

- **只出 Apple 芯片（aarch64）的包**，不做 universal：Intel Mac 越来越少，universal 让包大一倍。
- **磁盘映像**（app 加 `/Applications` 链接），不做 pkg：拖进去就装好，不需要安装器的权限。CLI 照 Linux 另出
  tar.gz。
- **手写 bundle 和脚本**，不用 cargo-bundle：要的只有 `Info.plist`、二进制、图标，签名公证的步骤也要自己控制。
- **最低 macOS 12**：gpui-fast-apple 按 12.0 编译 Metal 着色器，链接和 `LSMinimumSystemVersion` 跟它一致。
- **公证两轮**：先把 app 和 CLI 一起提交，票据钉在 app 上，从磁盘映像拷出来离线也能打开；再提交磁盘映像并钉上票据。
  只公证磁盘映像会少一轮，但拷出来的 app 首次打开要联网查。
- 用 App Store Connect API key 认证 notarytool，不用 Apple ID 加专用密码：不绑个人账号，也不会碰到两步验证。
- 不加 entitlements：没有 JIT、不进沙盒，keyring 用的 generic password 不需要 entitlement。
- 图标在打包时由 `catmail-light.svg` 改 `viewBox` 渲染到 Apple 的 824/1024 网格，不另存一份 SVG，免得两份走样。
  resvg 用 `cargo install` 装固定版本，不用 Homebrew（版本不固定，依赖多）。
- 签名的 secrets 放在 `release` environment，只有手动运行（发布、`build_only`）用；PR 拿不到，ad hoc 签名，
  只检查打包流程。
- 暂不做自动更新（Sparkle 要维护 appcast 和 EdDSA 密钥）。

## 方案

- `packaging/macos/build-dmg.sh`：构建 GUI 和 CLI，检查只含 arm64、`minos` 12.0、只链接系统库；组装 bundle、
  渲染图标、签名（hardened runtime、时间戳，核对团队 ID）、公证并钉票据、`hdiutil` 打包（偶发的 "Resource busy"
  重试）、`spctl` 检查，输出到 `target/macos/`。`packaging/macos/Info.plist` 是模板。
- `.github/workflows/release.yml`：新增 `macos` job（`macos-15`），建临时钥匙串导入 `.p12` 和固定 sha256 的
  Developer ID G2 中间证书，按团队 ID 找出身份，交给脚本；publish 等它并下载全部 artifact。
- 文档：`docs/architecture/packaging.md` 新增 macOS 一节，发布一节提到顶层，两个平台共用；README 安装说明、路线图，
  `background.md` 的通知说明同步更新。
- 验证：actionlint、shellcheck 通过；本地用 resvg 渲染图标看过留白；`build_only` 在分支上实跑签名和公证，PR 的
  运行检查 ad hoc 路径。
