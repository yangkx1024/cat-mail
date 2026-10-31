# 在窗口右下角显示悬停链接的地址，右键可复制链接

- 日期：2026-10-31
- 类型：功能

## 背景

读信区的 HTML 正文里，链接文字和实际地址可能不一样（跟踪链接、钓鱼邮件）。需要像浏览器那样，指针放到链接上就能看到地址；右键链接时也要能复制它。

## 决策

- 先做过“停留 1 秒后在指针上方弹 tooltip”，试用后觉得太干扰阅读：提示盖住正文，还要等。改成浏览器状态栏的做法：指针下有链接就立刻在窗口右下角显示，不延迟。
- 接着试过显示完整地址、太长就换行，超长的跟踪链接会变成一大块。改成浏览器的做法：只占一行，最宽约窗口的三分之一。
- 省略方式没有直接用 GPUI 的 `text_ellipsis_middle`：它把宽度前后对半分，`paypal.com.verify.evil.ru/...` 这种长主机会被省掉结尾，而结尾才说明链接去哪。所以拆成两段：协议加主机（含用户名，`bank.com@evil.example` 能冒充主机）从开头省略，最多占六成宽；路径和参数从中间省略。
- 主机按 URL 标准解析（`url` crate，`catmail-gui` 新加的依赖，工作区里已有），不按字面找第一个 `/`：浏览器把 `https://evil.example\@accounts.bank.example/login` 里的 `\` 当路径分隔符，实际去 `evil.example`，按字面拆会把 `@accounts.bank.example` 算进主机，从开头省略时正好省掉真正的主机（Codex review 指出）。显示的也是解析后的地址，即浏览器会去的地址；Copy link 仍复制原始 `href`。
- 显示内容直接取 `PageView::hovered.link`，不另存状态。页面在指针下移动、或者重新排版时，不会有新的 mouse move，悬停结果会过时：起初只在滚轮事件时清掉，后来改成页面位置变了就重问，但图片在固定高度的盒子里挤开链接时页面位置不变（Codex review 指出）。最后让悬停结果记着它问的是哪个点、哪个排版（`Probe`），指针在页面上时 `measure` 每帧比一下，不同才重问。页面的 `on_hover` 改用 `InputModalityIndependent`：默认模式在按键（以及测试里的滚动）后会当指针已离开，把刚发出的查询清掉。
- 右键菜单不再用渲染时捕获的链接：菜单在右键之后才构建，悬停查询又是异步的，移到另一个链接上马上右键会复制成上一个链接（Codex review 指出）。改成构建时读页面的状态，只有悬停结果正是对按下的点、当前排版问的才用它。还没答到时，起初直接不给 Copy link，但快速右键就总缺这一项（Codex review 又指出）；现在菜单先照常打开，同时按按下的点异步问一次，答到是链接就用 gpui-kit 的 `PopupMenu::rebuild` 把 Copy link 补到最上面（答案一般几毫秒内就到，菜单项下移一格的时机很短）。没在构建时阻塞等答案：渲染线程可能正忙着画分块，不在 UI 线程上等它。菜单项抽成了 `page_menu`，打开时和补上时共用。
- Copy link 只在右键时指针在链接上才出现，放在菜单最上面，和 Copy / Select all 之间用分隔线隔开。复制的是原始 `href`，不管能不能打开。
- 纯文本正文没有链接，不处理。

## 方案

- `crates/catmail-gui/src/reader/page.rs`：`link_address` 用 `deferred(anchored())` 把地址钉在窗口右下角，`split_link` 拆出主机；`Probe` 记下悬停结果问的点和排版，`measure` 发现不同就重问，`on_hover(false)` 时调 `unhover`；右键菜单构建时用 `known_link_at` 取按下处已知的链接，未知就异步问了再 `rebuild`。
- `docs/architecture/gui.md` 的“链接”一节补上这些。
- 测试：
  - `a_link_under_the_pointer_shows_its_address_and_copies`：地址出现在窗口右下角，移到普通文字或页面外后消失，右键链接后 Copy link 复制出 `href`。
  - `a_link_scrolled_under_the_pointer_shows_its_address`：指针不动、页面滚动后链接移到指针下，地址出现。
  - `a_link_an_image_pushes_from_under_the_pointer_goes`：图片在固定高度的盒子里挤开链接后，地址消失。
  - `the_menu_copies_the_link_under_the_press`：从链接 A 移到 B 马上右键，菜单补上 Copy link 后复制的是 B，不是 A。
  - `a_link_splits_after_its_host`：普通地址、带用户名、反斜杠、`mailto:` 和相对地址。
  - 后面三个在对应修改前都会失败。另外截图看了短地址和超长钓鱼式地址的显示。
