# 拆掉 deslop-please 报出的几处小模块循环

- 日期：2026-11-25
- 类型：重构

## 背景

用 [deslop-please](https://github.com/deterministic-machines/deslop-please) 0.1.4 扫描工作区（macOS、默认 features），共 82 个问题。其中 7 个是两三个模块之间的循环，起因都是一个辅助函数或类型放错了模块，挪一下就能消除，风险低，先单独处理；`Engine`、store 表之间级联、GUI 顶层这些需要先做设计的问题留到后面。

## 决策

每处都按"哪个模块在下层"来定归属，而不是挪到哪里都行：

- cli：`compose --send` 用 `outbox::send`，`outbox` 又用 `compose::save_to_server`。`save_to_server` 做的是 `replay_pending`，提示也用 `ops::refused_drafts` 和 `NEXT_FLUSH`，归到 `cmd::ops`。
- frontend html：页面图片 URL 的格式（`IMAGE_SCHEME`、`image_base`）由 `page` 决定，`page` 的 CSP 里本来就写着这个 scheme；`pages` 只负责解析，所以两者挪到 `page`。考虑过让 `body_page` 接收 base 参数，但那样要改一批测试，收益不大。
- frontend list：`Select` 描述的是读完一页后选中项怎么变，归到 `selection`，`model` 引用它。
- gui mailbox：`back_button` 操作的是列表的 `Selected`，归到 `mailbox::list`；`fact` 是一个通用的"名称：值"行，composer 的转发头部也在用，归到 `form`（已有带标签的 `field`）。
- mcp：`BaseUrl` 只用于 `prepare_upload` 给出的上传 URL，和 `uploads::PATH` 放在一起；`http` 设置它，`tools` 读取它，二者都依赖 `uploads`。
- store：`migrate::to_latest` 只需要一个写事务，直接在 `Connection` 上开 `IMMEDIATE` 事务，不再借用 `Store::transact`。`found == known` 时事务直接回滚，与之前提交空事务的效果相同。
- testkit：`mime` 只有假 IMAP 服务器用，移到 `imap/mime.rs` 作为子模块；`wire` 随之改为私有。

## 方案

涉及 `crates/catmail-cli/src/cmd/{compose,ops,outbox}.rs`、`crates/catmail-frontend/src/html/{mod,page,pages}.rs`、`crates/catmail-frontend/src/list{.rs,/model.rs,/selection.rs}`、`crates/catmail-gui/src/{form.rs,mailbox/*,composer/quote.rs,reader/view.rs}`、`crates/catmail-mcp/src/{http,uploads,tools/drafts}.rs`、`crates/catmail-store/src/migrate.rs`、`crates/catmail-testkit/src/{lib.rs,imap/}`。只移动代码，行为不变。

验证：重跑 deslop-please，这 7 处消失且没有新增（82 → 75）；`cargo fmt`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 均通过。
