# cargo deny 接受 imap-proto 的 git 来源

- 日期：2026-10-29
- 类型：修复

## 背景

#124 起 imap-proto 用 `[patch.crates-io]` 固定到上游 main 的一个 commit（未发版的 0.18.0，为了
djc/imap-proto#209）。`deny.toml` 的 `allow-git` 为空，`audit.yml` 的 sources 检查从那以后在每个 PR 和
main 上都失败，当时作为"项目未发布，先接受"记下。一直红着的检查会盖住新的问题（新加的 git 依赖、被禁用的
crate、许可证），PR 上也分不清是不是自己引起的。

## 决策

- 把 `https://github.com/djc/imap-proto` 加进 `allow-git`，带原因和 #130，而不是放宽 `unknown-git`：
  仍然只有逐个列出的仓库能用，别的 git 依赖照样失败。
- 只放行这一个仓库，不用 `allow-org`（整个 djc 组织）。
- 0.18.0 发版、换回 crates.io 时一起删掉（#130 的清单已相应修改）。

## 方案

- `deny.toml`：`allow-git` 列出 imap-proto 的仓库。
- 根 `Cargo.toml` 的补丁注释、`docs/architecture/protocol.md`、README 的 CI 说明、AGENTS.md 同步改写。
- 验证：本地 `nix develop -c cargo deny --locked check bans licenses sources` 三项通过。
