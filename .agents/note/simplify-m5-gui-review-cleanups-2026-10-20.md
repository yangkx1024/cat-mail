# M5 GUI 分支的精简与复用整理

- 日期：2026-10-20
- 类型：重构

## 背景

对 `m5-gui` 分支（相对 `main`）做了一轮复用 / 精简 / 效率 / 层次审查，整理出重复代码、可推导的状态、
热路径上的多余工作，以及一处放在共享路径里的残留标记。

## 决策

- "移走后打开下一封"的标记改为按邮件身份（`expect_removal(id)`），只在那封邮件仍是选中项时生效。
  原来只记位置：移动失败或用户换了选中后标记仍在，之后无关邮件在服务器上消失时，列表会跳到旧位置并打开
  （可能顺带标为已读）。考虑过等调用返回 `Ok` 再记标记，但引擎的事件可能先到、刷新先完成，那时已无从
  得知原位置，所以仍在调用前记，只是绑定到邮件。
- `DraftsChanged` 只重数该账户的草稿，不再整体重读侧栏：写信窗口每次停顿都保存，每次都重读所有账户的
  文件夹和发件箱是浪费。
- 未做：搜索结果随每批同步重跑的节流（改变行为，需要单独设计）；附件内容逐个抓取改为一次
  `fetch_sections`（要新的引擎接口）；`PendingOpsChanged` 时整队列读取并逐条 dismiss（只在大量被拒
  改动时才明显）；转发正文里的地址拼接沿用 `Address` 的 `Display`（用于显示，不加引号），不改用
  `format_addresses`。

## 方案

- `layout.rs` 新增 `window_options`、`empty`、`badge`，替换主窗口与写信窗口、列表 / 移动选择器 /
  读信区空状态、发件箱状态与"Imported"标记里的重复代码；写信窗口的表头行建立在 `layout::labelled` 上。
- `main.rs` 抽出 `init_app`，GUI 测试与 `main` 共用同一套初始化（测试原来漏了文字大小等全局动作）。
- 写信窗口：删掉无人触发的 `DiscardDraft` 动作；`Field` 只表示地址栏，初始焦点改为 `focus_body`；
  `untouched()` 合并两处判断；`open_window` 返回 `Result<()>`；草稿不见时用 `Error::DraftNotFound`。
- `Preferences` 去掉与 `Writer.dir` 重复的 `config_dir`；设置对话框的账户列表直接读 `Mailbox` 的
  （观察它的变化），删去自己的加载任务与删除后的手动重读。
- `AccountFolders` 在构建时记下按角色命名的文件夹，`paths()` 不再重算。
- `Row::Draft` 改为 `Rc<Draft>`，渲染右栏时不再每帧深拷贝草稿；"在浏览器中打开"的内嵌图片拷贝挪到
  后台线程。
- 核心：草稿进入发件箱时调用 `draft_gone`，与其文档一致。
- 测试：`a_removal_still_waiting_doesnt_move_on_from_another_message`（去掉身份检查即失败）。
  文档：`docs/architecture/gui.md` 的"读与改"和事件一节。
