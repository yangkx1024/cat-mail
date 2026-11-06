# 搜索框的规则移进 ListModel

- 日期：2026-11-06
- 类型：重构

## 背景

精简移进 frontend 的桌面模型时跳过了一项：搜索框的规则仍在 gui，没有不依赖 UI 的测试。规则有三条：
- 停顿后没有新按键才搜索（按键计数）；
- 文字去掉首尾空白后为空就回到选中的地方，否则换成 `Search(text)`，和正显示的一样就不动；
- 本地结果显示 `SERVER_SEARCH_DELAY` 后再问服务器，用户已经换了来源就不问、问到的也不用。

gui 为最后一条拿着 `ListModel::generation()` 到处比。

## 决策

- `ListModel::typed()` 记一次按键（清空搜索框也算），返回 `Typed`。`paused(typed, text, place, listing)` 说停顿后
  换成什么来源；期间又有按键，或者和正显示的一样，就返回 `None`。
- `server_search(listing)`：显示的是搜索结果时，返回一个 `ServerSearch`，带着文字和当时的世代；`ServerSearch::run`
  调 `MailClient::search_server`。`is_shown(&search)` 判断用户是否已经离开这次搜索；`found_on_server(&search, found,
  listing)` 直接返回要做的读取（期间正在读，就合并到读完后的那次），gui 不再自己 `refresh`。`generation()` 不再公开。
- 计时（`SEARCH_DELAY`、`SERVER_SEARCH_DELAY` 的 `timer`）仍在 gui：用的是 nsh 的定时器，模型只做判断。

## 方案

- `catmail-frontend`：`list/model.rs` 加 `Typed`、`ServerSearch`、`typed`、`paused`、`server_search`、`is_shown`，改写
  `found_on_server`。测试：期间又有按键时不搜、首尾空白去掉、没输入时回到选中的地方（或者什么都不显示）、和正显示的
  一样时不动；不是搜索结果就不问服务器、什么都没找到不重读、正在读时合并到下一次、换了来源后不问也不用。
- `catmail-gui`：`mailbox/list/mod.rs` 的 `typed`、`clear_search`、`search_server` 只剩计时和调用；`ListState` 去掉
  `typed`。
- gui.md 的邮件列表一节。
