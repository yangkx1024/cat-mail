# 两个登录表单共用提交流程

- 日期：2026-10-23
- 类型：重构

## 背景

issue #95：GUI 的登录页 `AddAccount::sign_in` 和添加发件地址的 `AddIdentity::add` 几乎逐行相同
（进行中就返回、校验字段、设忙碌、`spawn_in` 调 `MailClient`、完成后取消忙碌并 emit 或显示
"Couldn't sign in: …"），忙碌、错误、`Task` 三个字段和"Cancel + 主按钮"那一行按钮也各写了一份。

## 决策

- 在 `login_form.rs` 加 `Submit`：`start(submit, request, done, ..)` 收下已校验的请求
  （`Result<Future, &'static str>`，`Err` 是字段问题），`error(id, cx)` 渲染错误行（原来的
  `login_form::error` 并入），`actions(cancel, submit)` 渲染按钮行。表单按 `ServerFields::render` 的写法传一个 `fn(&mut T) -> &mut Submit`，让任务完成时
  找回状态。
- 按钮行由表单把 `Button`（id、文字、点击）传进来，`Submit` 只加 disabled / primary / loading：元素
  id 不变，测试照旧能找到；也不必为可选的 Cancel 再设计参数。
- 忙碌仍用一个 `bool`，没改成 `Option<Task>` 在任务里置空：那要靠在任务自己的回调里丢掉它的
  `Task`，本 crate 别处没这样用过。
- 进行中按 Enter 仍先读字段、构造请求再丢弃，开销可以忽略，换来表单里没有提前返回的分支。

## 方案

- `crates/catmail-gui/src/login_form.rs`：`Submit`。
- `mailbox/add_account.rs`、`settings/dialog/identity.rs`：去掉各自的提交状态，只剩字段和要调用的
  `add_account` / `add_identity`。
- `docs/architecture/gui.md` 登录页一节补一句。

验证：行为不变，靠现有 UI 测试（`signs_in_from_the_window` 的错误显示、
`cancel_is_off_once_signing_in_has_started`、身份表单的添加测试）覆盖；`cargo fmt`、
`clippy -D warnings`、全部测试通过。
