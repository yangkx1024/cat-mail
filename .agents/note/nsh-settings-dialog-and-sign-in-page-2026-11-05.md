# nsh 版的设置对话框、主题和登录页

- 日期：2026-11-05
- 类型：功能

## 背景

`nsh-spike` 分支的 ROADMAP P7、P8：nsh 版要有和 gpui 版一样的设置（外观、主题、文字大小、读信、账户与发件
身份）、内置和导入的主题，以及没有账户时的登录页。

## 决策

- **主题**：nsh 的主题是 shadcn 的 token。把选中的浅色、深色两个 gpui-kit 主题按颜色键映射成 shadcn `cssVars`
  的 light / dark 两节（`theme.rs`），经 `AppView::set_theme_overrides` 给每个视图；shadcn 没有的颜色（`warning`、
  `link`、`selection`、六个色相）作为应用自己的 token。nsh 读不了的颜色值（gpui-kit 自己的 `blue-500` 这类名字）
  略过，该 token 保持 nsh 的默认。旗标、头像、发件箱徽章原来写死的颜色改成从主题混出来（`tint`，和 gpui 版
  `colors.rs` 一样按明暗取不同比例）；发件箱的"未确认送达"和侧栏的警告图标用 `warning`，不再借用 destructive。
- **设置在进程里只有一份**（`shell.rs` 的 `static Mutex`），任何视图都能改（`update_settings`）：写文件在后台
  线程，按 `SettingsFile` 的队列取最新的；主题和文字大小在 GTK 线程上设给所有 `AppView`；外观只能在视图自己的
  线程上 `set_color_scheme`，所以每个视图（主窗口、写信窗口）在根组件里 `follow` 设置的变化。窗口打开时读当前的
  设置，不再用启动时的那份。
- **对话框**：nsh-base 的 `dialog()` 加自己画的外框（`form::modal`），和原来的命名对话框、"Move to"一样。三节：
  Appearance、Accounts、Reading；General（常驻、新邮件通知）的功能随 P9 才有，先不放开关。导入的确认放在
  Appearance 这一节里替换主题列表，和 gpui 版一样，不在对话框上再叠对话框。文字大小用原生 `popup_menu`。
- **命名对话框**从侧栏搬到 `form.rs`，请求变成闭包，文件夹的新建 / 改名和发件身份的改名共用；身份的名字可以
  为空。
- **登录页**在主窗口里替换三栏：账户读完且为空时自动打开（不能取消），Settings › Accounts 的"Add account"也能
  打开（能取消）。添加发件地址是设置之上的对话框。字段用信号保存，任一字段按 Enter 提交，进行中 Cancel 不可用。

## 方案

- 新文件：`theme.rs`（映射与应用自己的颜色）、`settings.rs`（对话框、`follow`、快捷键）、`login.rs`（登录页、
  发件地址的表单）、`form.rs`（对话框外框、按钮、字段、提交、命名对话框）。
- `shell.rs`：设置、设置文件、主题都是进程级的；所有视图的弱引用；`update_settings`、`follow_settings`、
  `with_themes`、`themes_changed`。`main.rs` 把 `SettingsFile` 交给 `shell::run`。
- `app.rs`：登录页与三栏切换，设置、发件地址、命名三个对话框；`off_thread`（读要导入的文件）。`sidebar.rs`：
  底部的 Settings 按钮。`list.rs`、`outbox.rs`、`reader.rs`：颜色取自主题。`icons.rs`：新图标。
- 调试用的环境变量 `CATMAIL_NSH_SETTINGS`（`accounts`、`reading` 选节）启动即打开设置，便于截图。
- 验证：nsh 的 clippy、测试通过（新增内置主题的每个 token 都设了、读不了的颜色被略过）；用空的数据和配置目录
  启动，截图确认登录页、设置的 Appearance（浅色、深色）和 Reading。登录、添加发件地址、改设置后写回文件和
  导入主题要连真实服务器或用户的文件，交给用户手动试。
