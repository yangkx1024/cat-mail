# 整理 frontend、testkit，把 gui 里不需要工具包的逻辑移到 frontend

- 日期：2026-11-17
- 类型：重构

## 背景

#320 的可选项：`catmail-frontend/src/list.rs` 里选中去哪的几样只有 `ListModel` 用；`write_atomically`、
`remove_if_present` 放在 `settings.rs`，却被 `instance.rs`、`autostart/*`、`theme/themes.rs` 用；
`catmail-testkit/src/imap.rs` 有 2.8k 行，`TlsMode`、`trait Stream`、`fn lock` 在 imap.rs 和 smtp.rs 各定义了一遍；
`catmail-gui` 里还有两处不需要工具包的逻辑（托盘、通知对主窗口邮箱的请求，web view 是否已长到页面的高度），
测试要带着 nsh 才能跑。

## 决策

- **`list/selection.rs`**：`Reselect`、`reselect`、`Removal`、`Reveal` 和它们的三个测试原样搬过去；`Removal`、
  `Reveal` 从 `pub(crate)` 收窄到 `pub(super)`（只有 `list::model` 用）。测试里的 `message(id, None)` 换成只建无日期
  行的本地辅助。
- **`fs.rs`**：crate 私有（`mod fs;`，`pub(crate)` 函数），四处调用改成 `crate::fs::…`。顺带补了两个测试（写入时建
  目录、覆盖且不留临时文件；删不存在的文件算成功，删目录失败）。
- **testkit**：`TlsMode`、`Stream`、`lock` 放进 `tls.rs`，`lock` 改成对 `Mutex<T>` 泛型，两个服务器共用。
  `imap.rs` 拆成 `imap/{mod,wire,search}.rs`：`wire` 是读命令（内联字面量）、分词、`quote` / `literal`（`mime.rs`
  也用，所以 `wire` 是 `pub(crate)`）；`search` 是 `SearchKey`、`search_key`、`gm_raw_matches` 等；`set_contains`、
  `header_end`、`filter_headers` 也被 FETCH 用，留在 `mod.rs`，`search` 从 `super` 取。公开 API 不变。
  `header_end` 上一段错位的文档注释归还给 `filter_headers`。
- **`Asks`**：请求本身（`Ask`）和“没有邮箱时记着、交给正在跟随的邮箱、记住上次在哪”的模型移到
  `catmail_frontend::mailbox::Asks`（`futures-channel` 本就是 frontend 的依赖，发送端留在模型里，语义一字不变）。
  gui 的 `shell` 只留进程级的 `static ASKS: Mutex<Asks>` 和三个薄函数 `ask`、`follow_asks`、`left_at`，主窗口
  关闭时调 `Asks::closed`。考虑过只搬 `left_at` 的插入逻辑、通道留在 gui：模型就测不到“邮箱停止跟随后请求退回等待”
  这一条，放弃。
- **`tall_enough`**：连同它依赖的 `MIN_HEIGHT` 移到 `catmail_frontend::html`（`page.rs`，紧挨着报高度、报画好的
  `FromPage`）；gui 的 `web::MIN_HEIGHT` 改为重新导出，其他视图不用改。`reader/mod.rs` 只改了调用处（写全路径，
  不动 import）和删掉函数及其测试，以免和另一条分支在 `show_remote_images` 附近的改动冲突。

## 方案

- frontend：新增 `src/list/selection.rs`、`src/fs.rs`、`src/mailbox/asks.rs`；改 `list.rs`、`list/model.rs`、
  `settings.rs`、`instance.rs`、`autostart/{linux,portal}.rs`、`theme/themes.rs`、`mailbox.rs`、`html/{mod,page}.rs`、
  `lib.rs`。
- testkit：`src/imap.rs` → `src/imap/mod.rs`，新增 `src/imap/{wire,search}.rs`；改 `smtp.rs`、`tls.rs`、`mime.rs`。
- gui：`shell/mod.rs`、`mailbox/mod.rs`（`Ask` 改从 frontend 引入）、`reader/mod.rs`、`web.rs`。
- 文档：`docs/architecture/background.md`（`Asks` 在 frontend）、`docs/architecture/gui.md`（`tall_enough`）。
- 验证：拆前拆后去掉 `use`、空行后的代码行排序比较，testkit 和 list 只差三处重复定义合成一处、可见性和测试辅助；
  测试数：frontend 185 → 191（`Asks` 3 个、`fs` 2 个、`tall_enough` 从 gui 移来 1 个并补了边界），gui 16 → 15
  （移走的那个），testkit 3 → 3，core 376 → 376，全部通过；fmt、clippy `-D warnings` 通过。
