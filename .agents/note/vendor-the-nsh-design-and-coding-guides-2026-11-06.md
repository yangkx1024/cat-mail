# 随 catmail-gui 附上 nsh 的设计与编码指南

- 日期：2026-11-06
- 类型：功能

## 背景

GPUI 版删掉时，`crates/catmail-gui/.claude/skills/` 里附带的 gpui-kit 编码指南和设计指南也跟着删了，
nsh 版没有对应的指南，AGENTS.md 里"改 GUI 前先读"的规则也没了。nsh 仓库自己带了两个 skill。

## 决策

- 从 nsh 仓库的 `skills/` 原样拷贝 `nsh-design-guides`（Design Guides）和 `nsh`（Coding Guides、Rust 组件、
  响应式、测试），取 workspace 钉住的同一个 rev（`f8cfd1a`），指南和用的 nsh 版本一致；和原来 gpui-kit 的做法相同，
  不改内容，附 Apache-2.0 许可证。
- 两个都附上，不只附设计指南：原来也是两个一起附的，编码指南里的组件、响应式和测试部分改 GUI 时同样要看。
- README 里写明哪些不适用：catmail-gui 把 nsh 当库用、自带外壳，没有 `nsh.json`，不用 `nsh` 命令行，
  `nsh` skill 里 TSX、`nsh init`、打包、更新的部分用不上。

## 方案

- 新增 `crates/catmail-gui/.claude/skills/`（`nsh/`、`nsh-design-guides/`、`LICENSE-APACHE`、`README.md`，
  README 记上游 commit 和更新方法）。
- AGENTS.md 加回规则：改 `catmail-gui` 前先读这两个 skill。
- 验证：`diff -r` 与 cargo 取下的 nsh 源码（同一 rev，工作区干净）一致。
