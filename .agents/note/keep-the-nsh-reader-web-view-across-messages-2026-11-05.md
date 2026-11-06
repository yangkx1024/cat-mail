# nsh 版读信区换邮件时复用同一个 web view

- 日期：2026-11-05
- 类型：修复

## 背景

量常驻内存时发现，catmail-nsh 每打开一封邮件就新起一个 WebKit 渲染进程。每个进程约 100 MB PSS，还要重新建一次
bwrap 沙箱和 `xdg-dbus-proxy`；旧进程随即退出。原因是 web view 每次都被销毁、再新建，而 WebKit 在 web view
销毁后就结束它的渲染进程。重建发生在两层：

- `app.rs` 决定右栏显示什么时，读的是整个选中的 id，换一封邮件就重建整个读信区。
- `reader.rs` 在新邮件 Loading 期间把邮件视图换成空 view，web view 被销毁，Open 时再新建一个。

## 决策

- 右栏只在选中行的种类变化时重建（`Pane`：邮件、草稿、发件箱）。
- 读信区 Loading 时保留邮件视图（`Shown::Message`），并且像浏览器换页那样继续显示上一封：表头、页面和 web view
  的高度都不变，WebKit 也会把旧页面留到新页面画出来。新邮件到了再一次性替换，同时滚回顶部。最初的做法是
  Loading 时把内容设为透明、把高度清零，可邮件背景和主题背景不同时，中间会露出主题背景，肉眼能看到闪一下。
  只有加载超过 250 ms（`STAYS_SHOWN`，比如要从服务器下载正文）才隐藏旧内容，免得长时间停在上一封上。
  Loading 期间清掉 `html`，免得"在浏览器打开"打开的是上一封。滚回顶部按上次建页的邮件判断（`Page::shown`）：
  同一封邮件因为外观变化重新建页时，不滚动。
- web view 的背景设成页面的背景（`Reading::page_dark`：暗色页面用主题的暗色背景，亮色页面用亮色背景）。WebKit
  换页时，新页面画出来之前可能先露出 web view 自己的底色，默认是白色。nsh 在
  元素有不透明背景时，会把它设成 WebKit 的底色。第一封邮件来之前，底色按当前外观取。
- 读信区第一次打开、或者加载太久被隐藏时，表头和附件先用 opacity 隐藏，web view 上盖一层与页面同色的遮罩，
  等页面在它自己的高度上画好再一起显示：页面加载完和每次视口尺寸变化后，脚本连续两次 `requestAnimationFrame`
  再发 `p:<innerHeight>`；视口撑到页面高度（`tall_enough`）后才显示，最多等 1 秒（`SHOWN_ANYWAY`）。之所以要看
  高度，是因为第一封邮件时 web view 从 40 px 撑到页面高度，WebKit 在新尺寸上画好之前，新露出的区域是深色空白。
  只等两帧不够：那个信号比撑高来得早，窗口打开后的第一封邮件仍会在文字后面闪一下深色。
  遮罩要解决的问题在 nsh：它只在 web view 的属性更新时（`WebHost::set`）才把背景色交给 WebKit，新建 web view
  时样式可能还没算出来，WebKit 就先用白底，暗色下第一次打开纯文本邮件时能看到闪一下。起初是给整块内容（包括
  web view）设 opacity 0，等 `r` 再显示，但 nsh 不画透明的 view，WebKit 那段时间没有被画上去，显示出来时
  先是一帧很深的空白，表头也比正文先出来。所以 web view 一直照常画，只在上面盖遮罩。web view 也改成等第一个
  页面到了才建，不再先加载一个空白页，免得空白页发来的消息被当成新页面的。
- 读信区和它的 web view 随主窗口一直存在，开窗时就先起好渲染进程（预热）。原因是 web view 改成常驻之前，开窗
  后第一次点开邮件要新起 web view：录屏量出从选中到内容出现约 13 帧（约 217 ms），大部分花在 WebKit 启动渲染
  进程、建沙箱上。没选中邮件时，web view 里放一个空白页（`page::blank_page`），读信区改成绝对定位放在右侧，并设为
  `invisible`：仍然排版、有尺寸，只是不画、不接受点击。不能用 `display: none` 或宽度 0：nsh 只在原生控件有尺寸时
  让它可见（`set_child_visible(has_size)`），WebKit 也只往可见的 web view 里加载页面。代价是开着主窗口时总有
  一个渲染进程（约 100 MB PSS），关窗常驻时它随窗口退出。
- 页面带编号（`<meta name="catmail-page">`），bridge 的 `r`、`h`、`p` 都附上它，读信区只认最后建的那个页面
  （`Page::number`）。预热用的空白页、上一封邮件的页面晚到的消息，就不会被当成新页面的。
- 已知：换邮件时，表头在新邮件到了那一刻就换，正文要再过约 3 帧（约 50 ms）、等新页面画好才换，中间新表头下面
  还是上一封的正文。看起来不明显，这次没改。

## 方案

- `app.rs`：`Pane` 与 `computed` 出的种类；读信区一直挂着，没选中邮件时用 `style_when` 改成绝对定位加
  `invisible`。
- `page.rs`：页面编号、`blank_page`；bridge 发 `r:<n>`、`h:<n>:<px>`、`p:<n>:<px>`，测试覆盖解析和编号；
  `reader.rs` 的 `tall_enough` 有单测。
- `reader.rs`：`Shown` 与 `computed` 出的显示状态；web view 的 `bg` 和遮罩跟着 `page_dark`（`page_background`）；`page_painted`；Loading 时只改 `id` 和
  `state`，超过 `STAYS_SHOWN` 才设 `hidden`；`Page::scroll` 和 `Page::shown`，换了邮件时 `scroll_to` 顶部。
- 验证：后台按秒采样进程树。连续换好几封邮件时，渲染进程的 pid 一直不变，PSS 从约 100 MB 涨到约 140 MB。
  用 `gpu-screen-recorder` 按 60 帧录屏，逐帧比较读信区的亮度和几个像素：开窗后点开第一封纯文本邮件，表头和
  正文在同一帧出现，背景颜色一帧都没变；从选中到出现约 2 帧（约 33 ms），改之前约 13 帧。开窗后、还没选中邮件时，
  渲染进程已经在了。切换显示、滚回顶部、远程图片和切换外观，用户在界面上逐项看过。
