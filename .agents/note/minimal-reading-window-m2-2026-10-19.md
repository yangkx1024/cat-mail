# M2：最小读信窗口（catmail-gui）

- 日期：2026-10-19
- 类型：功能

## 背景

M2 里剩下的一项。README 把最小读信窗口提前到 M2，是为了一拿到正文就用真实邮件验证
gpui-kit 的 HTML 渲染（表格、`cid:` 内嵌图片、中文排版），而不是等到 M5 才发现它不够用。
同时按用户要求，把 gpui-kit 仓库的 skills（编码规范、设计规范、GPUI 参考）放进 GUI crate。

## 决策

- **新 crate `catmail-gui`，只依赖 `catmail-core` 和 `gpui-kit`**（不直接依赖 `gpui`，避免
  链接两份）。单个二进制，测试放在模块里；`anyhow` 只在这个二进制里用。
- **范围**：账户 / 文件夹侧栏、邮件列表、读信、保存附件、同步按钮、"在浏览器打开"。不做
  搜索、写信（M3/M5）。添加账户的登录页见 `gui-sign-in-form-2026-10-19.md`。界面文案用英文，与 CLI 和引擎的错误信息一致；本地化留到 M5。
- **正文默认纯文本**（架构文档原有决定），可切换到 HTML，切换在邮件之间保留。
- **HTML 用 `gpui_base::text::TextView`**：component 层的包装没有暴露 `image_source`；主题
  样式由 component 安装的默认值提供，不需要自己拼。
- **图片策略**：只显示邮件自带的图片（`cid:`、`data:`）；其他一律返回立即失败的
  `ImageSource::Custom`，不发请求。`cid:` 解析与引擎判断"内嵌"共用 `content_id_from_url`
  （从 proto 的 `cid_references` 抽出、core 重新导出），保证两边对"哪张图被引用"的判断一致。
  考虑过加"加载远程图片"开关：GPUI 默认没有 HTTP 客户端，且这是隐私决定，留到以后。
- **链接只放行 http / https / mailto**：gpui-kit 默认会把任何链接交给 `open_url`。
- **"在浏览器打开"**：写一个独立页面，`cid:` 换成 `data:`、加 CSP 和 `no-referrer`，所以
  浏览器同样不加载远程内容。架构文档原来写的 CSP 有 `img-src cid:`，浏览器不认 `cid:`，改为
  先内嵌再只允许 `data:`。页面留在临时目录（浏览器在之后才读）。
- **图标**：默认资源只有组件用的图标，用 `icon_assets!` 只嵌入额外用到的 9 个，而不是整个
  Lucide 目录（`AllAssets`）。
- **测试**：UI 集成测试用 `#[gpui_kit::test]` 驱动真实窗口，数据来自假 IMAP 服务器（复用
  `testenv/fixtures/`，另加一封营销邮件式的 `crates/catmail-gui/fixtures/newsletter.eml`，
  只给 GUI 用，以免改动 Dovecot 测试的邮件数）。截图工具是 `#[ignore]` 测试，用 WGPU 离屏
  渲染，不碰桌面。
- **锁文件**：开启 `gpui-kit/test-support` 后，Cargo 会把 `dep?/feature` 里的可选依赖
  `gpui-fast-*` 算进锁文件，它们精确 pin 的 `cocoa 0.26.0`、`unicode-properties 0.1.3` 与
  已锁的新版本冲突。把这两个降到 pin 的版本（与 gpui-kit 自带的 `Cargo.lock` 一致）。
- **/simplify 整理**（复用、简化、效率、层次四个角度的 review）：
  - 图片的判断与替换从 GUI 里扫描 `src="` 文本，改为 proto 里随清洗同一次解析完成
    （`image_sources`、`embed_inline_images`）。写测试时发现 ammonia 不会再检查属性过滤器返回的
    值，所以 proto 不接受调用方给的替换字符串，`data:` URL 由它自己构造。`data:` 解码改用
    `data-url` crate（gpui-kit 同款，已在依赖树里），去掉了一份重复的 `percent_decode`。
  - 每帧的浪费：正文、头部、附件列表在打开时一次准备好，正文以同一个 `SharedString` 交给视图；
    图片按 URL 先查缓存再解码；"远程图片"的判断移到后台线程；浏览器页面复用已加载的内嵌图片。
  - 侧栏和列表的重读改为合并请求（同一时间最多一次，期间的事件只记"过期"），而不是替换任务：
    替换只取消 GUI 侧的 future，已交给引擎的读取照样会做。没用定时器去抖：GPUI 测试调度器里
    定时器要手动推进时钟。
  - 登录页的 IMAP / SMTP 字段合并为 `ServerFields`；表单只检查端口格式，其余交给引擎的校验；
    core 抽出 `connection::login`，`connect` 与 `check_login` 共用。
  - 没采纳：与 CLI 共用 `addresses` / `size` / `date`（前端各自负责展示，GUI 之后要本地化）；
    CLI 的 `account add` 也先检查登录（CLI 允许离线添加，测试也依赖这一点）；把订阅包装成不暴露
    tokio 的类型（`subscribe` 本来就返回 tokio 的 `Receiver`，属于既有接口）；内嵌图片并发获取、
    各账户的文件夹并发读取（数量很少，且多半是本地读取）；`MailClient::standalone_html`（页面外壳
    和 CSP 只是 GUI 的兜底展示）。
- **codex review 修复**（均先写出不修就失败的测试）：
  - SVG 图片（`data:image/svg+xml` 或 `cid:` 部分）交给 GPUI 后由 usvg 以默认选项渲染，它会把
    嵌套的 `<image href>` 当本地路径读文件，绕过 `image_source`：能显示本地图片，或读
    `/dev/zero` 耗尽内存。读信区不再渲染 SVG（`raster_format`）。
  - 刷新后选中的邮件不见了（删除、UIDVALIDITY 改变）时，列表只是静默清掉选中，读信区还显示
    已删除的邮件，点附件会失败。现在发出 `ListEvent::Cancel`（与 Escape 相同），`Mailbox` 清空
    读信区；Escape 取消选择时也一样。
- **skills 放在 `crates/catmail-gui/.claude/skills/`**：Claude Code 在编辑该 crate 时自动
  发现；AGENTS.md 里写明位置，其他 agent 也能找到。原样复制，附 Apache-2.0 许可证和上游
  commit。

## 方案

- `crates/catmail-gui/src/`：`main.rs`（参数、打开引擎、Quit、数据目录打不开时显示错误窗口）、
  `mailbox.rs`（根视图、侧栏、同步、事件）、`message_list.rs`（`ListDelegate`、分页、刷新时
  按 MessageId 保持选中）、`reader.rs`（读信、内嵌图片、附件保存、浏览器页面）、`html.rs`
  （图片 / 链接策略、浏览器页面）、`folders.rs`（按分隔符建文件夹树，补出服务器没列出的
  父级）、`format.rs`、`assets.rs`、`test_support.rs`。
- `catmail-proto`：`content_id_from_url`、`image_sources`、`embed_inline_images`（复用清洗的
  ammonia 配置）；`catmail-core` 在 `catmail_core::html` 下重新导出它们，另导出 `RecvError`。
- `flake.nix`：GUI 的链接库（fontconfig、freetype、libxkbcommon、libxcb）和运行时 dlopen 的库
  （wayland、vulkan-loader、libGL、libX11，经 `LD_LIBRARY_PATH`）。`ci.yml`：Linux 上装对应的
  开发包。
- 文档：README（状态、用法、开发依赖）、架构文档"GUI"与"测试环境"、AGENTS.md。

验证：`cargo fmt`、`clippy -D warnings`、全部测试通过。新增 GUI 测试 14 个（另有 1 个忽略的截图工具）：纯逻辑（文件夹
树、日期、图片 / 链接策略、浏览器页面的内嵌与 CSP、图标都已嵌入）和 2 个 UI 集成测试（自动选中
收件箱、点击打开、切换 HTML、方向键切换邮件；另一处同步后窗口靠事件出现新邮件、未读数更新且
选中不变）。对后者做了变异测试：去掉 `MessagesChanged` 的列表刷新、刷新时不恢复选中，测试都会
失败。proto 新增 `content_id_from_url` 的单测。手动：对临时 Dovecot（加一封营销邮件）用 CLI
同步后打开窗口，无报错；离屏截图检查了纯文本、带内嵌图片的 HTML 和营销邮件。

**发现**：段落、标题、数据表格、链接、`cid:` 图片、中英混排都正常，但**布局表格会把整封邮件
压成一段**；去掉外层布局表格后同一封邮件显示正常。下一步是渲染前展开布局表格，见架构文档。
截图里中文字形比拉丁字母浅、`SEMIBOLD` 看不出加粗，是离屏渲染的字体回退，真实窗口里没有核对。
三平台 CI（尤其 macOS / Windows 上编译 GPUI）在本地无法验证。
