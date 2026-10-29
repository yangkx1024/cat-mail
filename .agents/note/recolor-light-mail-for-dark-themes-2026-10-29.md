# 深色主题下转换为白底设色的邮件

- 日期：2026-10-29
- 类型：功能

## 背景

切到深色主题后大部分邮件仍是白底，而 Gmail 网页版会把它们显示成深色。原因是 2026-10-22 的设计：
只要正文为白底设了颜色（哪怕一个 `color:#333`，`ColorUsage::Light`），读信区就保持白底（Apple Mail
的做法）；营销邮件和大多数 HTML 邮件都属于这一类。顺带发现深色页面上邮件没设颜色的链接一直是
`#0000ee`：Blitz 自带样式表用 `a[href]` 设链接色，优先级高于读信区的 `a`。

## 决策

- 像 Gmail 一样自动转换，不加单封切换开关或设置项（与用户确认）。
- 在源码上改写颜色，而不是在 Stylo 层：计算后的样式不可变且每次重排会重新计算，Stylo 的 forced
  colors 是丢掉作者颜色而不是转换。清洗器已经逐记号重写样式表，加上颜色映射成本小。
- 按用途转换，而不是整体反转：背景只变暗、文字只变亮、边框反转。整体反转会把深色页眉和白字变成
  浅色页眉和黑字；按用途转换保证文字与背景的对比方向不变。在 OKLab 里做，保留色相，品牌色仍可辨认；
  端点取主题的背景色和文字色，纯白背景与读信区融为一体，主题的色调也带进灰色。
- 文字用更短的反转范围：主题两色之间的范围比黑白窄，等比例反转会让 `#666` 这类灰字对比过低。
- 改写放在 `catmail-proto`（与清洗共用分词和 ammonia 配置），由 `catmail-render` 的
  `Appearance::recolor` 钩子在文档线程上执行，不占 UI 线程，`PageView` 也不需要新的异步状态。
- 自带深色样式的邮件（`Adaptive`）不转换，用它自己的。图片不变（透明底的黑色图标可能看不清，
  Gmail 也一样）。

## 方案

- `catmail-proto/src/mime/dark.rs`：`DarkPalette`、`Role`（按属性名定用途，不取颜色的属性不改，
  避免 `font-family: Tomato`）、OKLab 转换与降饱和度的色域映射、旧式颜色属性的解析。
- `css.rs`：`sanitize` 接受可选调色板，`copy_rest` 在取颜色的属性值里（包括渐变等嵌套函数）识别
  十六进制、颜色名、`rgb()`、`hsl()` 并写回 `#rrggbb` / `rgba()`；`recolor_style` 改写 `style` 属性。
- `html.rs`：`recolor_html` 用带调色板的清洗器再清洗一遍，改写 `style`、`bgcolor`、`color` 属性和样式表；
  经 `catmail_core::html` 导出。
- `catmail-render`：`Appearance::recolor`（`Recolor` 类型），Blitz 打开文档前调用。
- `catmail-gui/src/reader/page.rs`：深色主题下页面一律为深色，`Look::recolor` 在 `Light` 时为真；
  链接选择器改为 `a[href]`。
- 文档：`docs/architecture/gui.md`、`bodies.md`。
- 验证：dark.rs（黑白落到主题色、背景只变暗文字只变亮、色相和透明度保留、旧式属性）、css.rs
  （各种写法、嵌套、条件块、非颜色保持不变、style 属性）、html.rs（属性和样式表、无颜色时不变、
  不能逃出属性）、render 测试（`recolor` 钩子生效）、GUI 单元测试 `a_dark_page_takes_the_themes_colors`
  （链接取主题色，去掉 `[href]` 时失败；白色单元格变成主题背景）、GUI 集成测试
  `shows_html_dark_in_a_dark_theme`（为白底设色的邮件在深色下取主题背景，旧行为下失败）；截图检查
  深色主题下的 newsletter。
