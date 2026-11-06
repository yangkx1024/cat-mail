# 发件箱右栏的 From 显示条目实际的发件身份

- 日期：2026-11-06
- 类型：修复

## 背景

#250：发件箱条目右栏的 From 一行显示的是账户主地址（`account.email`），可条目记着它要用的发件身份
（`OutboxEntry::identity`）。账户加了第二个地址、写信时选它、发送后条目留在发件箱（离线或发送失败），
右栏的 From 就显示成主地址，像是要用错的地址发出（实际发送没错）。

## 决策

- 规则放进 `catmail_frontend::outbox::facts(entry, identities)`，给出右栏的 To / From / Queued，gui 只画出来。
- From 的叫法复用写信窗口的 `composer::senders`（有名字时 `名字 <地址>`；同一地址经两个服务器时后面带服务器），
  两处一致，不另写一套。传进去的是所有账户的身份，和写新邮件时 From 的选项一样：只传本账户的，两个账户有同一地址时
  写信窗口带服务器、右栏不带。
- 显示的是身份现在的名字；排队后改了名，已经生成的邮件仍是旧名字。少见，只是显示，不处理。
- 身份被删时怎么显示：不用处理。存储层在发件箱还有条目用着某个身份时拒绝删除它（`identities.rs` 的 `delete`），
  所以条目的身份总在；只有侧栏还没读到账户时 From 为空，和原来账户没读到时一样。
- 参数取身份列表而不是 `AccountEntry`：只需要这一项，测试也不用造整个账户。
- 考虑过让 `OutboxEntry` 带上 `envelope_from`：只有地址，没有名字，也分不出同一地址经两个服务器，不如按身份找。

## 方案

- `crates/catmail-frontend/src/outbox.rs`：新增 `facts`；测试 `from_is_the_identity_it_goes_out_as`
  （非默认身份显示它自己的名字和地址；同地址不同服务器带服务器；未读到时为空）。
- `crates/catmail-gui/src/mailbox/outbox.rs`：`page` 改用 `facts`，传所有账户的身份。
- `docs/architecture/gui.md` 发件箱一节写明。
- 验证：macOS 上 fmt、clippy `-D warnings`、全部测试通过。
