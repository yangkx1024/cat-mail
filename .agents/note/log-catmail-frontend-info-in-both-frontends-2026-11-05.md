# 两个前端的日志文件都记 catmail-frontend 的 info

- 日期：2026-11-05
- 类型：修复

## 背景

量 nsh 版常驻时的内存时发现，关窗后日志里没有 `returned freed memory`，尽管采样显示 trim 确实做了：主进程的
anon 从 104 MB 降到了 90 MB。原因是 `memory::release_freed` 在 bc503d5 里从 catmail-gui 搬进了
catmail-frontend，日志的 target 随之变成 `catmail_frontend::memory`。而日志文件默认只放行 warn、引擎各 crate
的 info 和前端自己的那部分（`catmail_gui` / `catmail_nsh`），所以这行被过滤掉了。同一次搬过去的
`no tray icon` 也一样，gpui 版因此丢了这两行。

## 决策

在两个前端传给 `logging::init` 的过滤里都加上 `catmail_frontend=info`。frontend 里 info 级别的日志只有这两条，
都很少出现，不会刷屏。也考虑过把它们降到 warn，但它们说的是正常情况，不是警告，所以没有这样做。

## 方案

`catmail-gui/src/main.rs`、`catmail-nsh/src/main.rs` 的过滤字符串，以及 `docs/architecture/logging.md` 的说明。
没有加测试：过滤是传给 subscriber 的字符串，测试只能重复写一遍同一个字符串。验证办法是常驻后看日志里有
`returned freed memory`。
