# 清理没人读的主题键、只写不读的列和没接线的 API

- 日期：2026-11-18
- 类型：重构

## 背景

#332（来自 #319 的全仓库审查）：主题推导写了一大批 GUI 不读的 GPUI Kit 组件键；schema 里有几列只写不读；
`connect_for_search` 写好了没接线；单实例协议发了几个没人读的显示环境变量；还有几处只给测试用的公开项、
重复的测试辅助函数和过时的注释。删之前都以全工作区编译为准。

## 决策

- **主题键**：推导只写 GUI 读的 41 个键（`catmail-gui` 的 `theme.rs` 的 `TOKENS` 加 `Swatch::of`），去掉
  `button.*`、`tab*`、`table.*`、`title_bar.*`、`scrollbar.*`、`chart.bullish` 等。不再追求写出去的文件对 GPUI
  Kit 完整兼容：仓库里已经没有 GPUI Kit，旧主题文件里多出来的键照样加载、忽略。对比度测试只测 `Roles`，
  它们都还在用，保留。
- **只写不读的列**：删掉 `messages.gm_thrid`、`in_reply_to`、`internal_date` 和 `outbox.updated_at`，改
  `0001_init.sql`（项目未发布，labels.md 的惯例），没有加迁移。已有的本地库不用重建：这几列还在，但新代码
  不碰它们，`outbox.updated_at` 有默认值。会话分组还没定怎么做，按 storage.md「现在不预留」，以后要用再加列；
  协议层照常解析 X-GM-THRID 和 In-Reply-To（它是通用的协议库）。`NewMessage.internal_date` 留着当 `date` 的
  后备。`modseq`、`keywords_json` 没动：issue 只是记了一笔，改它们会改变行为。
- **`connect_for_search`**：接上，不删：搜索框获得焦点时调用，ROADMAP 那一项就完成了。范围是 `None`（每个账户），
  跟 `search_server` 一样。
- **单实例协议**：缩成 `Activate { activation_token }`，`VERSION` 升到 2：旧的 app 读不了新请求，版本号能让
  它给出明确的说法，而不是解析失败。
- **`Db::messages()`**：只补了文档。要让 `db.messages().x(); db.messages().touched_labels()` 编译不过，得把
  `touched_labels` 改成随 7 个写方法的返回值带出来，会碰到所有调用方，超出这个 issue 的范围。现在没有哪处这样误用。

## 方案

- `catmail-frontend/src/theme/derive.rs`：`colors()` 重写，模块注释改了；`translucent` / `over` 的注释改成说
  GUI 的 30% 选区上限。gui.md 补一句派生写哪些键。
- `catmail-store`：schema、`NewMessage`、upsert 的 SQL；`Outbox` 那几个状态转换只为写 `updated_at` 才收的
  `now` 参数去掉了（`begin_send`、`begin_save`、`mark_unknown`、`requeue`、`postpone`、`fail`、`save_later`、
  `give_up_saving`），`send.rs` 跟着改。`BlobStore::open_blob` 删掉；`contains`、`Messages::count` 改成
  `#[cfg(test)]`；`Folders::sync_states` 改成 `pub(crate)`，文档注释改正。
- `catmail-core`：不再再导出 `EVENT_CAPACITY`、`DEFAULT_DRAFT_UPLOAD_DELAY`、`DEFAULT_GC_INTERVAL`、
  `KeyringSecrets`（改成 `pub(crate)`）。`large_message()`、`ALL_MAIL` 挪进 `tests/common/mod.rs`。
- `catmail-html`：新的 `without_sheet`（有测试），撰写器引用原信时用它，不再先构建带作用域的样式表再丢掉。
  `catmail_core::html` 用它替换了 `nested` 的再导出（crate 外只有这一处用 `nested`）。
- `catmail-gui`：搜索框的 `on_focus` 调 `connect_for_search(None)`；GPUI Kit 当视觉参照的注释改成直接写尺寸或
  意图；`main.rs`、`shell/mod.rs` 跟上新的 `Request`。
- 文档：storage.md、labels.md、gui.md、background.md、ROADMAP。
- 验证：`cargo fmt`、`clippy --workspace --all-targets -D warnings`、`cargo test --workspace`（macOS）。
  搜索框获得焦点时的提前登录只验证了能编译，没有在运行的应用里试过。
