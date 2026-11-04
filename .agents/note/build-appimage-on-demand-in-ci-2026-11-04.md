# CI 手动构建 AppImage，不发布

- 日期：2026-11-04
- 类型：功能

## 背景

`build-appimage.sh` 拒绝 Nix 构建的二进制（解释器和 glibc 来自 `/nix/store`），NixOS 上本地打不了能
分发的包。release workflow 手动运行就是发布，只构建的路径只有改了打包的 PR。

## 决策

- 给 `release.yml` 的 `workflow_dispatch` 加 `build_only` 布尔输入，不另写一个 workflow：构建 job 只有
  一份，PR 的构建本来就是"版本 job 输出当前版本、ref 为空、publish 跳过"，`build_only` 走同一条路径。
- 任何分支都能跑；同时填了 `version` 直接失败，免得以为发了版。
- concurrency 组按 `build_only` 分开：同在 main 上时，只构建的运行既不该排在发布后面，也不该顶掉排队中
  的发布（同组只保留一个待运行的）。

## 方案

- `.github/workflows/release.yml`：新输入、concurrency 组、版本 job 在 `build_only` 时提前输出当前版本。
- `docs/architecture/packaging.md`：发布一节补上用法。
- 验证：actionlint 通过；推分支后 `gh workflow run release.yml --ref <分支> -f build_only=true` 实跑。
