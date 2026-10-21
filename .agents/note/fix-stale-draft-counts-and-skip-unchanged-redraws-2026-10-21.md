# 修正过期的草稿数，内容没变时不重绘

- 日期：2026-10-21
- 类型：修复

## 背景

PR #59 之后复核遗留的 issue：

- #64：事件接收端 lag 时，按账户记的 `DraftsChanged` 次数不变，晚到的旧草稿计数会盖住侧栏重读得到的新值。
- #65：最近一次草稿计数失败时静默返回，更早的计数已被丢弃，草稿数停在旧值直到下一次 `DraftsChanged`。
- #61：侧栏和列表每次重读都 `notify`，即使读回来的和正显示的一样；同步时每 200 ms 重绘一次整个窗口。
- #60：`background_sync_waits_for_a_draft_to_settle` 等的是服务器有副本，断言的是本地已记下上传，负载高时偶发失败。

## 决策

- 草稿数的新旧判断抽成纯逻辑 `refresh::DraftCounts`（与 PR #59 的其他决策逻辑一致），表驱动测试。
- lag 时给已有计数的账户各加一，而不是清空表（清空后计数从 1 重来，可能和在途的旧计数撞号）；必须在重读侧栏之前加，否则侧栏的结果也被当成过期。
- 计数失败改为重读侧栏，而不是重试计数：重试只降低概率。侧栏也失败时会弹通知，可以接受。
- #62（游标分页）和 #63（被拒绝改动的专门事件）不在这次：都是跨 store / core / 前端的接口改动，现状的代价小（漏行由随后的刷新补回；每次本地改动多一次读），单独做。

## 方案

- `crates/catmail-gui/src/mailbox/refresh.rs`：`DraftCounts`（`changed` / `missed` / `is_latest` / `read_is_latest`）及测试。
- `crates/catmail-gui/src/mailbox/mod.rs`：`on_engine_event` 收到 `None` 先 `missed()` 再读；`count_drafts` 失败且仍是最近一次时 `load_folders`；`AccountEntry` 派生 `PartialEq`，`set_accounts` 遇到相同的列表直接返回。
- `crates/catmail-gui/src/mailbox/message_list/mod.rs`：`Row` 派生 `PartialEq`，从头重读得到相同的行和 `has_more` 时不替换、不 `notify`（行为空时照常，骨架屏要随 `loading` 消失）。
- `crates/catmail-core/tests/drafts.rs`：等待条件里加上 `on_server`。
- 新窗口测试 `reading_again_what_hasnt_changed_redraws_nothing`：模拟 lag 触发侧栏和列表重读，断言两者都没 `notify`（去掉侧栏的比较时失败）。
- 文档：`docs/architecture/gui.md` 的草稿数与刷新部分。
