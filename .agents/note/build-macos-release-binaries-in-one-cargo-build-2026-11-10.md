# macOS 发布包一次 cargo build 构建两个二进制，CI 缓存 resvg

- 日期：2026-11-10
- 类型：重构

## 背景

release workflow 的整体耗时由 macOS job 决定（10–17 分钟，Linux 的 CLI 和 Flatpak 各在 11 分钟以内）。
它的日志显示 `build-dmg.sh` 先 `cargo build -p catmail-gui`（10m35s），再 `cargo build -p catmail-cli`
（3m33s），第二次重编了 114 个 crate：两次各自合并 feature，tokio、rustls、rusqlite 直到 catmail-core 都对不上，
只能再编一遍。另外每次运行都从源码 `cargo install resvg`，约一分半。

## 决策

- 一次 `cargo build -p catmail-gui -p catmail-cli`，与 Flatpak manifest 的做法相同。CLI 因此也带上 GUI 那边
  打开的依赖 feature，行为不变。
- 不把 Linux CLI 并进 Flatpak job：Flatpak 在沙箱里用 SDK 的 Rust 和 GNOME runtime 的 glibc 构建，产物在
  glibc 2.35 上跑不了，原生 CLI 也没法和它共用编译结果；合在一起只能串行，反而更慢。
- resvg 用 `actions/cache` 缓存 `target/macos/tools/bin`，key 是脚本里固定的版本号（workflow 从脚本里读出来，
  不另写一份）。缓存里的版本不对时脚本照旧重装。PR 写的缓存只有它自己的 ref 读得到，签名的发布运行只会用到
  main 上的运行装的 resvg。不缓存 Rust 依赖：缓存进签名的构建要多想一层，收益也要另外量。

## 方案

- `packaging/macos/build-dmg.sh`：两次 `cargo build` 合成一次。
- `.github/workflows/release.yml`：macOS job 读出 `resvg_version`，加 `actions/cache` 步骤。
- `docs/architecture/packaging.md`：macOS 一节写明一次构建和 resvg 缓存。
- 验证：actionlint、shellcheck；分支上 `build_only` 实跑，对比 macOS job 的耗时，再跑一次确认缓存命中。
