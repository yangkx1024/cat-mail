# 搜索框加清除按钮

- 日期：2026-11-19
- 类型：功能

## 背景

搜索框没有办法一下清空：要手动删掉文字，焦点还留在输入框里，列表的单键快捷键（E、X……）不能用。

## 决策

- 与 AppKit 的 `NSSearchField` 一样，只在有输入时显示右端的实心 x-circle（灰色，悬停变前景色）。
- 点它清空文字、立即回到所选位置（复用 `ListModel::paused`，不等 200 ms 的停顿），并让输入框失去焦点。
- 不给搜索框绑 Escape：Escape 已经全局用来关上读信区，两者冲突。

## 方案

- `mailbox/list/mod.rs`：`settle`（搜索框的文字落定后显示什么，停顿后的 `typed` 与清除按钮共用），`clear_search`
  返回它的 `Typed`；`clear_search_field` 是两者加 `blur`；`search_field` 里按 `SearchText` 是否为空显示按钮。
- `icons.rs`：`Icon::XCircleSolid`（`assets/heroicons/solid/x-circle.svg`）。
- 文档：`docs/architecture/gui.md` 搜索一段。clippy、GUI 测试通过；效果要在运行的 App 里看。
