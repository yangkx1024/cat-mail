# 把真实服务器测试并入 Linux 的 CI 测试 job

- 日期：2026-10-19
- 类型：重构

## 背景

CI 在 Linux 上有两个 job：Clippy & Test 和 Real-server tests（Dovecot、mailpit）。后者在
`nix develop .#testenv` 里用 nixpkgs 的 rustc 跑 cargo，用不上 rust-cache，每次从零编译
`catmail-core` 及其依赖：main 上一次运行（39271019984）编译 3 分 05 秒，测试本身约 2.5 秒。
而 Clippy & Test 的 `cargo test --workspace` 早已编译了同样的 `dovecot`、`submission` 测试
二进制，只是没有服务器的环境变量，测试直接跳过。PR 上 Linux 这边的耗时因此由 4 分多钟的
Dovecot job 决定。

## 决策

- **并入 check job 的 Linux 分支**，只有服务器（Dovecot、mailpit、openssl）来自
  `nix develop .#testenv`，cargo 仍用 job 自己的 rustup 工具链和缓存，真实服务器测试随
  `cargo test --workspace` 一起跑，不再单独编译。
- **服务器在 job 开头就后台启动**，拉取 dev shell（约 30 秒）与恢复缓存、clippy 重叠；测试
  之前再等它就绪并导出变量，服务器没起来时这一步直接失败。
- `CATMAIL_TESTENV_REQUIRED` 只在 Linux 写进 `GITHUB_ENV`：测试只看变量是否存在，在 Test
  步骤里用表达式给其他系统设空值也会被当成"必须有服务器"。
- 代价：真实服务器测试失败时显示为 Clippy & Test (ubuntu-latest) 失败，要看日志；不再用
  nixpkgs 的 rustc 编译一遍（原本也不是有意的检查）。仓库没有分支保护，不涉及必需检查改名。
- 考虑过给 Dovecot job 单独加缓存：nix 工具链的缓存要另占约 1 GB 的仓库缓存配额，而且仍然
  重复编译，不如合并。

## 方案

- `.github/workflows/ci.yml`：check job 在 Linux 上加 install-nix、后台启动服务器、等待并导出
  变量三步；删除 `dovecot` job。
- AGENTS.md、README、`docs/architecture/testing.md` 的 CI 说明随之更新。
- 用 actionlint 检查过工作流；以合并前这个 PR 的 CI 运行确认真实服务器测试确实执行（日志里
  `dovecot` / `submission` 的测试不是跳过）。
