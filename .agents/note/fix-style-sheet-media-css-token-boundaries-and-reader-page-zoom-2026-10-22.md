# 修复样式表的 media、CSS 记号边界和读信区页面的排版时序与文字大小

- 日期：2026-10-22
- 类型：修复

## 背景

Codex 审查 Blitz 读信区那次提交，指出四个问题，均已复现或读代码确认：

1. `css.rs` 把记号直接拼接写回，注释在分词时已丢：`background:u/**/rl("https://…")` 通过了
   “不能 fetch” 的检查，写出来却是 `url("https://…")`；再清洗一次整条声明被删，幂等性也破了。
   `1px/**/2px`、`div/**/p` 也被拼成别的东西。
2. 扫描 `<style>` 不看属性：`<style media="print">p{display:none}</style>` 让正文在屏幕上消失；
   `type="text/x-template"` 之类的模板也被当成样式表。
3. `PageView` 换了 source（内嵌图片到了）或重新打开后，`self.layout` 还是旧文档的，宽度没变时照样
   按旧高度画新文档的分块；排版回来时清掉了分块却没取消这些绘制任务，页面变高时原来不满一条的末条
   被装进来并拉伸到整条高度，之后也不会重画。
4. 设置里的文字大小和 Ctrl +/− 改的是窗口 rem，Blitz 页面完全不受影响。

## 决策

- 记号边界用 cssparser 自带的 `TokenSerializationType::needs_separator_when_before`，必要处插入
  `/**/`，与浏览器序列化规则一致；没有改成写回原文。
- 每个 `<style>` 单独解析，`media` 属性包成 `@media`。没有先拼接再包：一张表里多出的 `}` 会提前关闭
  包装的 `@media`，打印样式就漏到屏幕上。属性值不是媒体查询列表时整张不用，与浏览器的 `not all` 一致。
- 文字大小按浏览器的页面缩放处理（rem / 16，整页缩放），没有只改默认字号：邮件多用 px 定字号，只改
  默认字号几乎无效，也不符合设计规范里文字、间距共用一条缩放轴。

## 方案

- `crates/catmail-proto/src/mime/css.rs`：新增 `Css` 写入器记住上一个记号的类型；`sanitize` 接收
  `&[Element]`（文本 + media），`@media` / `@supports` 和 media 属性共用 `Sheet::conditional`。
- `crates/catmail-proto/src/mime/html.rs`：`scan` 按元素收集样式表，读 `media`，跳过非 CSS 的 `type`。
- `crates/catmail-gui/src/reader/page.rs`：`Opened` 记录本文档在当前宽度上的高度，只有它到了才画分块；
  重新打开和 `set_source` 时取消旧的排版任务；显示用的 `Layout` 带缩放比；文档按
  `scale × zoom` 打开、按 `width / zoom` 排版，指针坐标同样换算。
- 文档：`docs/architecture/bodies.md`、`docs/architecture/gui.md`。
- 测试：css 的 `keeps_tokens_apart_that_a_comment_separated`、`a_sheet_for_some_media_applies_only_to_them`，
  html 的 `style_sheets_keep_their_media_and_type`，page 的 `a_new_source_is_painted_once_it_is_laid_out`
  （去掉修复时稳定失败：第 1 条 376 vs 1024 设备像素）和 `the_page_zooms_with_the_text_size`。
