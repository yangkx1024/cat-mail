# GUI 的本地时间改从 nsh 的 locale_settings() 读时区，日期标题随时区和午夜重新分组

- 日期：2026-11-09
- 类型：功能

## 背景

issue #258。12 / 24 小时已经从 nsh 的 `locale_settings()` 读（会订阅），时区却还是 jiff 的
`TimeZone::system()`：`format::time` / `format::date` 经 core 的 `display::local` 读它，按天分组的
`Calendar::now()` 也读它。改时区后已显示的时间不变，要等行因为别的原因重建（jiff 的系统时区还有 5 分钟缓存），
日期标题也按旧时区分。另外标题只在读行时算，行没变时 `loaded` 不替换 `Listing`，窗口开过午夜，"Today"
会一直停在前一天。Flatpak 没有系统总线的权限，nsh 连不上 timedated，沙箱里根本看不到时区的改动。

## 决策

- `format::Clock` 从枚举改成结构体：时区（jiff 的 `TimeZone`）加 `Hours`（原来的 12 / 24 小时）。jiff 的
  `TimeZone` 可比较，能放进 nsh 的 `computed`。frontend 的 `time`、`date` 自己按它的时区格式化，不再经 core
  的 `display::local`；命令行和回复引用的"On … wrote"照旧用系统时区和 24 小时。
- 分组用的 `Calendar`（时区加今天）由调用方给：`Calendar::now(zone)`、`Calendar::at(now)`，`Rows::apply`
  带上它。`Listing` 记下分组用的 `Calendar`，`grouped_by` 判断是否过时，`regroup` 重新分组、重排条目；追加一页时
  如果 `Calendar` 已经变了，整个重新分组，免得前后两页按不同的时区或"今天"分。考虑过让 `Listing` 自己读时区，
  但 `ListModel::show` 新建 `Listing` 时拿不到，GUI 还得记得补上，不如每次应用行时传进来。
- 时区名怎么变成 `TimeZone` 是 frontend 的 `format::zone`：先查 jiff 的时区数据库，`GMT+01:00` 这类偏移（Android
  这样命名）直接换算成固定偏移，都不是才回退到 `TimeZone::system()`，免得这类名字又回到系统时区的缓存。
- GUI 的 `clock.rs` 每个窗口建一次两个 `computed`：`clock()`（读 `locale_settings()`，查一次时区）和
  `calendar()`（时区加今天）。行、头部、发件箱都读同一个，时区库只在设置变了时查，而不是每行每次重算都查；
  建在一个独立的 owner 下，和窗口的 globals 一样长，不属于先读它的那个视图。
- 午夜由 `clock.rs` 的计时器发现：等到下一天开始（`format::until_tomorrow`），最多等 10 分钟就再看一次，
  因为计时器不算电脑睡眠的时间，醒来时可能早过了午夜；没换天时 `calendar` 的值相等，不会通知。没有每分钟轮询，
  空闲时醒得少。列表只有一个 effect：`calendar()` 变了而 `Listing` 不是按它分组时调 `regroup`。
- core 的 `display::local` 改成 `pub(crate)`：只剩命令行的 `display::date` 和回复引用那行用它，frontend 不该再绕过
  `Clock` 的时区。
- Flatpak 加 `--system-talk-name=org.freedesktop.timedate1`：nsh 在 Linux 上从 timedated 读时区并监听它的改动。

## 方案

- `crates/catmail-frontend/src/format.rs`：`Clock { zone, hours }`、`Hours`、`Calendar::now(zone)` /
  `Calendar::at`、`until_tomorrow`、`zone`。测试：`zones_are_found_by_the_names_platforms_give`、
  `times_follow_the_users_clock`、`times_are_in_the_users_zone`（同一时刻在两个时区显示不同的日期时间）、
  `the_next_day_starts_at_local_midnight`。
- `crates/catmail-frontend/src/list.rs`、`list/model.rs`：`Days` 带 `Calendar`，`Listing::grouped_by` /
  `regroup`，`Rows::apply(listing, calendar)`。测试 `the_days_follow_the_users_zone_and_today`：同一组行在 UTC 和
  UTC+8 下的标题，换时区后那封信移到 Today 下，追加的一页按新时区重新分组，过了午夜 Today 变 Yesterday。
- `crates/catmail-frontend/src/{outbox,reader}.rs`：时钟改成借用。
- `crates/catmail-gui/src/clock.rs`：共享的 `clock()`、`calendar()` 和午夜计时器；`mailbox/list/mod.rs` 的
  `follow_the_calendar`；`rows.rs`、`outbox.rs`、`reader/view.rs` 改成传 `&clock()`。
- `packaging/linux/net.yangkx.catmail.yml`、`docs/architecture/packaging.md`（权限）、`gui.md`「邮件列表」、
  ROADMAP P3。
- 还要在 macOS、Linux（含 Flatpak）系统设置里各换一次时区，看列表时间、日期标题、读信区头部、发件箱的 Queued
  不重开窗口就更新。
