# 邮件列表、侧栏与设置对话框的界面修整

- 日期：2026-10-20
- 类型：修复

## 背景

- 邮件列表相邻两行紧贴在一起：选中行和悬停行的圆角背景连成一片，看不出是两项。
- 设置对话框左侧的分区（Appearance、Accounts、Reading）各自居中，图标没有对齐。
- 按钮上的 "…"（Settings、Add account、Remove、Import theme、Attach、Send again，以及 Move to、
  Delete draft 等的 tooltip），用户不需要。
- Appearance 的 System / Light / Dark 三个图标里，太阳看起来小一号。
- 主题卡片的名字被截断：Catppuccin 的 p、Light 的 g 下半截被切掉。
- 侧栏里账户的邮箱地址又小又淡，像分组标签；它说明下面是谁的邮件，应该是标题。
- 邮件列表要按日期分隔（Today、Yesterday，然后是日期）；未读邮件用加粗表示，不要左边的圆点。
- 改成加粗后用户机器上仍然不粗：整个界面在 Linux 上其实从来没有粗体，CJK 还偏细。

## 决策

- **列表行间距。** GPUI Kit 的 `List` 用虚拟列表渲染，不支持 `gap_y`（源码注释说明了原因），所以在
  `ListItem` 上加上下外边距。外边距在 `List` 包裹每行的 `div` 内，测量行高时会算进去，滚动和加载
  更多不受影响。
- **分区左对齐。** `Button` 的内容行总是居中（`.justify_start()` 只作用在外层，能改内层的 `content_style`
  不对外公开）。先用过在按钮里加一个 `flex_1` 空 `div` 把内容挤到左边，简化评审时改成 `SidebarMenu` /
  `SidebarMenuItem`：GPUI Kit 自己的设置页就用它画导航，天然左对齐、有选中态；元素 id 由它生成
  （`settings-sections-<序号>`），测试随之改。
- **按钮不加 "…"。** Design Guides 要求打开对话框或还要输入的命令加 "…"，这里按用户的要求一律不加；
  表示进行中的状态文字（Sending…、Saving…、Syncing…）不是按钮，保留。
- **太阳图标。** 三个 Heroicons 的网格和外框一样，但太阳是小圆加细短线，按钮尺寸下视觉上小一号。
  `Button` 统一设置图标尺寸，调用处没法单独放大，所以在 vendored 的 `sun.svg` 里绕中心放大 1.15 倍、
  线宽保持 1.5（Design Guides 允许有说明的视觉校正），文件里注释了原因。没有换别的图标库的太阳：
  整个窗口只用 Heroicons 一种风格。
- **主题卡片的名字。** 卡片是 `gpui_kit::base::Button`，它把行高设成字号（`relative(1.)`），名字的
  `truncate` 又隐藏溢出，下伸部分就被切掉；给名字一行 `rems(1.)` 的行高。
- **账户标题。** `SidebarGroup` 的标签样式写死（`text_xs`、70% 不透明度），没有接口可改，所以写了一个
  实现 `SidebarItem` 的 `AccountGroup`：邮箱用 `text_sm` 半粗体、侧栏前景色，菜单项沿用 `SidebarGroup`
  给子项的 id，元素 id 不变。

- **按天分组用 `List` 的 section**，不是插入标题行：`List` 要求所有行一样高（虚拟列表只测一行），
  section 标题单独测量。行仍放在一个从上到下的 `Vec` 里，选中、"移走后打开下一封"等逻辑照旧按序号，
  只在和 `List` 交互处用 `Days` 换算 `IndexPath`。分组合并相邻同一天的行，所以不依赖排序一定正确。
  有了日期标题，行里的日期只留时间（`format::time`），原来的 `short_date` 删掉。
- **Linux 上没有粗体的原因**：GPUI 的 cosmic-text 后端只在字族的独立字面之间按字重挑选，载入字面时固定用
  `Weight::NORMAL`，可变字体（用户机器上的 Noto Sans、Noto Sans CJK 都是）只能画出默认字重；用临时诊断
  测试对比各字面的字宽确认了 CJK 实际用的是可变的 Noto Sans CJK。三个方案里选了自带静态 Noto Sans
  （另两个：启动时探测已安装的有粗体的字族，在这台机器上会变成 DejaVu Sans，外观随机器变；或给 GPUI 打
  补丁、维护 fork）。CJK 没法自带（体积），所以设置回退字族，并在启动时排除可变字体（常规和粗体解析到
  同一字面的），否则排在前面的 Noto Sans CJK 会被选中。字体写进每个主题配置而不是全局主题，因为应用主题
  时会用配置里的字体覆盖；回退链用 `RootPlugin` 设在窗口根上，对话框和通知也能继承。
- **未读加粗**：去掉圆点那一栏，发件人和主题用 `FontWeight::BOLD`（原来是 SEMIBOLD，再加上圆点）；
  现在字重是唯一的提示，所以用更明显的粗体。

## 方案

- `crates/catmail-gui/src/mailbox/message_list/mod.rs`：`render_item` 给每行加 `my_0p5()`（上下各
  2 px，行间共 4 px，与设计指南中列表行的 4 px 节奏一致）。
- `crates/catmail-gui/src/settings/dialog/mod.rs`：分区导航改用 `SidebarMenu`。
- 去掉 "…"：`mailbox/sidebar.rs`、`draft.rs`、`outbox.rs`、`panes.rs`、`reader/view.rs`、
  `composer/view.rs`、`settings/dialog/themes.rs`、`accounts.rs`，以及 README、`gui.md` 里的引用。
- `crates/catmail-gui/assets/heroicons/outline/sun.svg`：放大后的太阳。
- `settings/dialog/themes.rs`：卡片名字的行高。
- `mailbox/sidebar.rs`：`AccountGroup`。
- `mailbox/message_list/mod.rs`：`Days`（分组与 `IndexPath` 换算）、`sections_count`、
  `render_section_header`；`rows.rs` 去掉圆点、未读加粗、只显示时间；`format.rs` 的 `day` / `time`。
  `fonts.rs`、`assets/fonts/noto-sans/`（含 OFL）、`theme/appearance.rs` 应用主题时套上 UI 字体、
  `main.rs` 初始化。单测：`day_headings_depend_on_how_long_ago`、`rows_are_grouped_by_day_and_indexed_both_ways`；GUI 测试
  里用方向键跨天切换邮件的用例照常通过。
- 都是纯视觉改动，没有加自动测试（测试只能拿到元素本身的位置，拿不到按钮内部的布局）；GUI 测试通过，
  并用 `screenshots` 测试离屏渲染检查了设置对话框和主窗口。
