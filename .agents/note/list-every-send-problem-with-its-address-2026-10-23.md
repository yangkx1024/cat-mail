# 发信问题带上地址，全部列出

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #93：`AccountStatus::send` 是按身份记的表，前端却只拿 `send_problem()`，即 id 最小的那一项；GUI
侧栏再自己去身份列表里查地址，查不到退回账户地址。两台 SMTP 服务器都出问题时用户只看到一个，看到哪个
取决于 id 大小。（issue 里提到的 `FlushReport.error` 已在 #80 改成按身份的 `errors`，这里不用再动。）

## 决策

- `send` 改成 `Vec<SendProblem { identity, email, problem }>`，按身份排列，去掉 `send_problem()`。地址在
  记录问题时就带上：引擎只在内存里记状态，`account_status` 是同步调用，不能再去查数据库；身份的地址也
  不能改，记下时的就是现在的。
- 只放地址（`email`），不放 `Address`：前端只说"发不出"的是哪个地址，显示名用不上。
- 一个已删除的身份没有地址可带，但它的连接结果本来就不记（`IdentityNotFound` 直接返回），所以给它留空。
- 侧栏仍只说第一个有问题的账户（与同步问题一致），但列出它所有发不出的地址。

## 方案

- types：`SendProblem`，`AccountStatus::send: Vec<SendProblem>`。
- core：`Activity::Send` 带上地址；`connection::SendAs`（身份和地址）用在 `SmtpLogin` 和连不上的路由里，
  `send.rs` 的 `connected` 按它记状态（日志仍只记 id）；`Engine::report` / `forget_identity` 按有序列表
  增删。
- GUI：侧栏"Can't send from a, b"，发件箱的提示改看 `send` 是否为空。
- 文档：`docs/architecture/sending.md`、`gui.md`。
- 测试：`tests/identities.rs` 的 `an_identity_that_cant_log_in_holds_back_only_its_own_messages` 断言两个
  问题都在、各带自己的地址；其余用到 `send_problem()` 的测试改读 `send[0]`。
