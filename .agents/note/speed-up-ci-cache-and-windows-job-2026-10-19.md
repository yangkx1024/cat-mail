# 加快 CI：缓存只在 main 保存、减小调试信息、Windows 关闭实时扫描

- 日期：2026-10-19
- 类型：重构

## 背景

Windows 的 Clippy & Test job 每次结束时 `Post Run Swatinem/rust-cache` 要 2.5～3.5 分钟。
看日志：整理约 50 秒，tar + zstd 打包 1.27 GB 约 2 分钟（Windows 上的 tar 处理大量小文件
很慢），上传约 17 秒；开头恢复缓存也要约 70 秒。

根本原因有两个：

- rust-cache 只要不是完全命中就会保存。PR 常改 `Cargo.lock` / `Cargo.toml`，于是每个 PR
  的每个系统都存一份，而 PR 的缓存只属于那个 PR，main 用不上。
- 仓库缓存上限 10 GB，当时已用 9976 MB（每个系统 1.1～1.7 GB，再加 #9、#10 的 PR 缓存）。
  main 的缓存被挤掉后，之后的运行只能部分命中、重新编译，又再保存一次。#9 的一次 Windows
  job 因此用了 12 分 50 秒。

## 决策

- **只在 main 上保存缓存**（`save-if: github.ref == 'refs/heads/main'`），PR 只恢复 main 的。
  代价：改了依赖的 PR，每次运行都要编译新增的依赖，直到合并后 main 重新保存。
- **CI 里用 `CARGO_PROFILE_DEV_DEBUG=line-tables-only`**：回溯仍有文件和行号，只是没有完整的
  变量调试信息。本地按 CI 的步骤（clippy + 构建测试，关闭增量编译）测：`target` 从 6.7 GB
  降到 4.3 GB（-36%），构建 198 秒 → 182 秒。只放在 CI 的环境变量里，不改 `Cargo.toml`，
  本地开发的调试体验不变。
- **Windows job 先关闭 Defender 实时扫描**：编译、链接和解包缓存时每个文件都会被扫描；
  runner 用完即弃，关掉没有安全上的顾虑。效果没有单独测量，这一步也打印
  `RealTimeProtectionEnabled`，便于确认它真的生效。
- 加了新的 `CARGO_` 环境变量后，rust-cache 的键会变，这个 PR 的运行和合并后 main 的第一次
  运行都拿不到旧缓存，是一次性的代价。

## 方案

只改 `.github/workflows/ci.yml`：全局 `env` 加 `CARGO_PROFILE_DEV_DEBUG`；check job 开头
加 Windows 专用的关闭实时扫描步骤；rust-cache 加 `save-if`。用 actionlint 检查过工作流。
验证看合并后的运行：PR 上 Windows job 的 post 步骤应接近 0，main 只保留三份缓存。
