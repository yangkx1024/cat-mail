# 交给浏览器的页面写到数据目录，引擎打开时清空

- 日期：2026-10-19
- 类型：修复

## 背景

读信窗口的"在浏览器打开"把整封邮件写成 `catmail-message-*.html` 放在系统临时目录，并
`keep()` 保留（浏览器在函数返回后才读）。没有任何地方删除它们：每点一次就多一个文件，邮件
正文一直留在磁盘上。复现：打开几封 HTML 邮件、点"Open in browser"，关掉窗口后临时目录里的
页面都还在。

## 决策

- **放到数据目录的 `handoff/`**（`Config::handoff_dir`），由 `MailClient::open` 在拿到锁之后
  清空。锁保证同一时间只有一个引擎、没有前端还在往里写，所以清空不会误删正在用的文件；崩溃
  留下的也在下次打开时清掉。共享的系统临时目录做不到这一点。
- **清理放在核心而不是 GUI**：目录和"打开时清空"的规则都由核心定义，锁也在核心；放在 GUI 的
  `main` 里要靠"打开窗口之前调用"的顺序，每个前端还得各自重复。最初是在 GUI 里做的，
  /simplify 的层次审查后移进核心。
- **名字用 `handoff` 而不是 `pages`**：目录是"交给其他程序的文件"，以后用系统程序打开附件也放
  这里，共用同一套清理。
- **打开时清空，而不是关闭时**：关闭时浏览器可能还在读；macOS 上应用退出不执行析构，靠
  `Drop` 删不可靠。代价是上一次会话的页面留到下次打开，它们和 `blobs/` 里的正文在同一个目录，
  没有多暴露什么。

## 方案

- `catmail-core`：`Config::handoff_dir()`；`client.rs` 的 `clear_handoff_dir`，在 `open` 里紧跟
  `lock_data_dir`（目录不存在不算错，其他错误只记日志，不让打开失败）。
- `catmail-gui/src/reader.rs`：`write_browser_page` 写到 `handoff_dir`（仍用 `tempfile`，仅本用户
  可读、文件名不重复）。
- 文档：架构文档"数据目录"和"GUI / 在浏览器打开"。

验证：core 单测 `handed_off_files_last_until_the_next_engine_opens`（拿不到锁的第二个引擎不动
文件；关掉第一个再打开后文件和目录都不在），去掉 `clear_handoff_dir` 调用时它失败；GUI 单测
`browser_pages_go_to_the_handoff_dir`（写到该目录、文件名不重复）。
