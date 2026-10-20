# 修复 GUI 同步保留选中测试的竞态

- 日期：2026-10-20
- 类型：修复

## 背景

PR #16 合并后 main 的 CI 在 Windows 上失败：
`mailbox::tests::shows_what_a_sync_brings_without_losing_the_selection` 最后断言读信区显示的邮件，
得到 `None`。测试点击邮件后，`Reader::open` 在引擎线程上异步下载正文，测试没有等下载完成就
开始同步，再等新邮件和未读数，最后直接断言。同步比正文下载先结束时读信区还停在 `Loading`。
Linux 上下载总是先完成，Windows runner 更慢才暴露。复现：在 `body::download_message` 开头临时
加 300ms 延迟，Linux 上稳定失败。产品代码没有问题。

## 决策

点击后先等读信区打开这封邮件再同步，和同文件 `a_message_gone_from_the_server_leaves_the_reader`
的写法一致。这样最后的断言检验的是"同步后选中和读信区没丢"，而不是下载与同步谁先完成。
没有选择在最后一步改成等待：那样分不清读信区是丢了又被打开，还是一直没丢。

## 方案

`crates/catmail-gui/src/mailbox.rs` 的测试在点击后加一次 `wait_until(shown_in_reader == Some(hello))`。
验证：上述临时延迟下，修改前失败、修改后通过（延迟未提交）。
