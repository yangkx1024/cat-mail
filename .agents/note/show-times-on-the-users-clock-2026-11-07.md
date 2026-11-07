# 时间按系统的 12 / 24 小时设置显示

- 日期：2026-11-07
- 类型：功能

## 背景

列表的时间、读信区头部的日期、发件箱的 Queued 一律是 24 小时（`%H:%M`），系统设成 12 小时的用户看到的和别的
应用不一样。nsh main 有了 `locale_settings()`（longbridge/nsh#976），能读到系统的 hour cycle，读它会订阅。

## 决策

- `catmail-frontend` 不依赖 nsh，所以在 `format` 里加自己的 `Clock`（`TwentyFourHour`、`TwelveHour`），由
  `catmail-gui` 的 `clock::clock()` 从 `HourCycle::twelve_hour()` 映射过来。`h11`、`h24` 很少见，分别并进 12 小时、
  24 小时，和 nsh-base 的 `TimeField` 一样。
- 时钟作为参数传给 `format::time`、`format::date`、`list::message_label`、`outbox::facts`、`Header::details`，
  不用全局变量：模型的测试不依赖进程状态。
- `Header` 原来在打开邮件时就把日期格式化成字符串，设置改了不会跟着变；改成存时间戳（`date: Option<i64>`），
  `details` 拆成地址（`addresses`）和按时钟生成的 `details(clock)`，在视图里格式化。列表行、发件箱页面本来就在
  响应式闭包里格式化，读 `clock()` 就会跟着变。
- 命令行（`catmail_core::display::date`）和回复引用的"On … wrote"那行照旧 24 小时：前者给脚本读，后者是发出去
  的内容，不该随本机设置变。
- 日期部分仍是 `YYYY-MM-DD`，没有按地区排列：要跟地区走得引入 ICU 一类的格式化库，这次不做。

## 方案

- `crates/catmail-frontend/src/format.rs`：`Clock`，`time`、`date` 带时钟，仍由 core 的 `display::local` 格式化；
  `date` 不再转出 core 的那个。新增测试 `times_follow_the_users_clock`（固定时区，直接测 `Clock` 的格式串）。
- `crates/catmail-frontend/src/{list,outbox,reader}.rs`：签名加时钟，`Header` 改成存时间戳。
- `crates/catmail-gui/src/clock.rs`（新）、`mailbox/list/rows.rs`、`mailbox/outbox.rs`、`reader/view.rs`。
- gui.md「邮件列表」加"时间"一条，模块表加 `clock.rs`；ROADMAP P3 记一笔。
- 验证：macOS 上 fmt、clippy、测试通过。还要手动在系统设置里切换 12 / 24 小时，看列表、头部、发件箱立刻跟着变。
