# 保留清洗后的样式表，读信区 HTML 跟随深色主题

- 日期：2026-10-22
- 类型：功能

## 背景

Blitz 支持 `prefers-color-scheme`，但清洗时整个去掉了 `<style>`，邮件自带的深色适配和大部分响应式
布局都到不了渲染器；深色主题下 HTML 正文一律是一块白底。

## 决策

- 样式表不交给 ammonia 放行：它不看 `<style>` 里面，写回时原样输出原始文本，安全性要押在它对变异
  XSS 边角情况的处理上（实测 4.2.4 处理住了 SVG / MathML / noscript 的已知写法）。改为自己取出、用
  cssparser 的记号逐条重写，输出中没有 `<`，样式表从结构上不能结束自己的元素。
- 白名单而不是子串黑名单：只丢掉会取资源的那条声明，不整块丢掉（营销邮件的样式表几乎都有一两个
  `url()`）；at-rule 只留 `@media` / `@supports`；去掉 `:has()` 和嵌套规则；限制大小和规则数。
- 深色只用于不设颜色或自带深色样式的正文；为白底设了颜色的保持白底（Apple Mail 的做法）。考虑过
  像 Outlook 那样反转亮度，会改变品牌色、让透明底的黑色图标消失，留作以后的手动开关。
- 资源耗尽：样式表让一条规则作用于所有元素，于是读信区对每个请求限时 10 秒（GPUI 的
  `with_timeout`），超时放弃这个文档；每个文档本来就有自己的线程，卡住的不影响别的。另加页面高度上限。
  先写过一版共享回复槽加看门狗线程的设计，`/simplify` 时换成了现在的做法，少了约 150 行共享状态。

## 方案

- `catmail-proto/src/mime/css.rs`：样式表重写与颜色判断；`html.rs`：分词取出 `<style>`（原始文本状态
  与树构建器一致，忽略 SVG / MathML 内的）、`clean` 把重写后的样式表放回开头（`sanitize_html` 与
  `embed_inline_images` 共用）、`color_usage`。
- `catmail-render`：`Appearance`（是否深色、画布色、读信区的用户代理样式表）、按画布色合成、
  `MAX_HEIGHT`；超时在 `PageView`。
- `catmail-gui/src/reader/page.rs`：`Look` 每帧比较主题颜色，变了按新外观重新打开文档；
  `message.rs` 计算 `color_usage`。
- 验证：css.rs 单元测试（保留规则与条件块、取资源的写法和转义、at-rule、`:has()`、嵌套、无 `<`、
  幂等、大小与规则数上限、颜色判断）；html.rs 测试（放回开头、闭合标签与变异 XSS 写法、只读真正的
  样式表、`color_usage`、浏览器页面保留样式表）；layout.rs 测试样式表在展开布局表格后保持不变；
  render 测试（画布合成、深色样式与外观、高度上限）；GUI 测试
  `shows_html_dark_when_the_message_allows_it`（深色主题下不设颜色的邮件取窗口背景色，切回浅色变白）。
