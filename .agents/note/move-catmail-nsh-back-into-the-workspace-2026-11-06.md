# 把 catmail-nsh 挪回主 workspace

- 日期：2026-11-06
- 类型：重构

## 背景

catmail-nsh 之前单独成一个 workspace：Blitz 会打开 taffy 的 `calc` feature，而 nsh-core 0.4.0 的
`sticky.rs` 里有个 match 没写 `Calc` 分支，两者放在一起就编译不过（longbridge/nsh#966）。上游已经在
main 上用 #1022 修好，但还没有发版。

## 决策

- nsh 的三个 crate 改用 git 依赖，钉在 main 的 f8cfd1a，写在根 `[workspace.dependencies]` 里。nsh 发版后
  换回 crates.io，同时把仓库移出 `deny.toml` 的 `allow-git`。
- `cargo deny` 还是过不了，原因都在 nsh 的依赖树里：许可证（webpki-roots 的 CDLA-Permissive-2.0，
  target-lexicon、winx 的 LLVM-exception），以及 nsh-updater 用的 zip 8 的四个 advisory（报到上游
  longbridge/nsh#1050）。这些没有加进 `deny.toml` 豁免：这个分支不进 main，等上游处理。

## 方案

- 根 `Cargo.toml` 去掉 `exclude`，新增 nsh、`glib-unix`、`resvg` 这几个依赖。catmail-nsh 全部改用
  `.workspace = true`，lints 也用 workspace 的，`main.rs` 开头加 `#![forbid(unsafe_code)]`。删掉它自己的
  `Cargo.lock` 和 `.cargo/config.toml`。
- CI 的 Linux job 加装 `libgtk-4-dev`、`libwebkitgtk-6.0-dev`。
- 验证：`cargo tree` 确认 taffy `calc` 已打开；workspace 的 fmt、clippy、测试全部通过。
