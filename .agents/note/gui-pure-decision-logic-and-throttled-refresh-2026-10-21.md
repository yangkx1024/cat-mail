# GUI 决策逻辑抽成纯函数，事件刷新节流，侧栏一次读完

- 日期：2026-10-21
- 类型：重构

## 背景

Issue #41：GUI 里最复杂的几段决策逻辑只能靠带窗口的测试触达，这类测试出过时序竞争；测试里"什么都没
发生"的断言用固定 sleep，点击 gpui-kit 内部生成的元素 id（`"ok"`、`"settings-sections-1"`），
`Lagged → reload` 和 `report_refused_changes` 没有测试。

Issue #42：每个 `MessagesChanged` 都重读整个侧栏（`list_outbox` + 每账户 `list_folders` + `count_drafts`，
1 + 2N 次查询）并重读列表已显示的行，同步一次几十个事件；搜索结果 500 条静默截断；`count_drafts` 和
`report_refused_changes` 的异步结果没有先后保护。

## 决策

- **节流而不是"同步中去抖"**：issue 建议 `syncing > 0` 时尾部去抖，但 `syncing` 只计窗口自己发起的同步，
  后台同步不算；去抖还会让单个改动（标记已读）也晚显示。改成首个事件立即读、之后 200 ms 一个间隔合并读，
  安静一个间隔就回到空闲。
- **侧栏快照进 core**：`MailClient::overview()` 一个读事务返回 `AccountOverview`（账户、文件夹、草稿数、
  发件箱）。`DraftsChanged` 也走它，去掉单独的 `count_drafts`（GUI 是唯一的调用者，一并从门面删掉）：
  侧栏读取本来就是一次一读、最后一次在最后一个变化之后，草稿数就不会被晚到的旧值盖住。连接状态在内存里、
  不在数据库里，不放进快照，结果落到窗口时才取，否则读取中途的 `AccountStatusChanged` 要再读一遍侧栏。
- **被拒绝的改动在 core 一次忘掉**：`MailClient::dismiss_settled` 在一个写事务里取出并删除失败 / 冲突的
  改动，不再由前端 `list_pending` 再逐条 `dismiss_pending`（1 + K 次写和 K 个事件）。
- **搜索仍跨所有账户**，所以任何文件夹的 `MessagesChanged` 都刷新搜索结果是对的；没有按 issue 的建议限定
  到某个账户（那要先把搜索本身改成按账户，是产品决定）。由节流限制代价。
- **搜索分页**而不是加"还有更多"提示：`SearchQuery` 本来支持 `offset`，和文件夹一样滚到底取下一页。
- **gpui-kit 的内部 id** 没法换成自己的（AlertDialog 的按钮、SidebarMenu 的项由 gpui-kit 命名；自己写
  对话框的 footer 要重做 OK / 键盘确认）。改为集中在 `layout::CONFIRM` 和 `settings::dialog::section_item`
  （按 `Section::ALL` 的位置算，不写死序号），升级时只改两处。按无障碍标签找按钮会和工具栏的 Delete 重名。
- **重复打开草稿**：原来靠建窗口前再查一次，测试只能 sleep 等第二次打开的读取结束。改为记下正在打开的
  草稿，第二次同步返回；任务挂在应用而不是发起的窗口上，那个窗口关了不会让草稿一直"正在打开"。

## 方案

- `catmail-types`：`AccountOverview`；`catmail-core`：`MailClient::overview`（测试
  `the_overview_counts_drafts_and_lists_the_outbox`）、`dismiss_settled`（store 的 `Ops::dismiss_settled`，
  测试 `settled_ops_are_dismissed_together_and_pending_ones_kept`），导出 `EVENT_CAPACITY`（lag 测试按它
  发事件）；`Folders::list`、`Accounts::list` 改用 `prepare_cached`。
- `mailbox/refresh.rs`（新）：`Refresh::after(event, shown)`（`None` 表示漏了事件，全部重读）和
  `Throttle`，表驱动测试。`mailbox/mod.rs`：`on_engine_event` 只做分派，`read_again` 按节流读并起计时；
  "一次一读、期间有变化再读一次"抽成 `refresh::Coalesce`，侧栏和被拒绝的改动共用；测试里
  `refresh_interval` 为零（测试时钟不自己走；让等待循环推进时钟会连带触发自动保存、搜索的计时器）。
- `mailbox/commands.rs`：`report_refused_changes` 合并请求，用 `dismiss_settled` 一次处理所有账户。
- `message_list/mod.rs`：`reselect` 纯函数（保持 / 下一封 / 消失），表驱动测试；搜索分页。
- `composer/lifecycle.rs`：`closing(CloseState)` 与 `steps(how, untouched, unsaved)`，表驱动测试；
  `composer/mod.rs`：`OpenComposers::opening`。
- 窗口测试：新增被拒绝改动被撤销并 dismiss、事件接收端 lag 后整体重读（临时把 `None` 改成不重读时该测试
  失败）；附件失败、重复打开两个测试改为等正面信号，不再 sleep。
- 文档：`docs/architecture/gui.md`（运行时与事件、写信窗口）、`docs/architecture/testing.md`。
