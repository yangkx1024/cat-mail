# 草稿的纯文本和 HTML 合成一个正文

- 日期：2026-11-14
- 类型：重构

## 背景

富文本写信加上 HTML 正文后，`DraftChanges` 有两个独立的 `Option`：`text` 和 `html`（#302）。正文是一个整体，
纯文本是 HTML 的另一种形式，两者只能一起换，但这条约束只写在注释里：只给 `html` 会被静默忽略，只给 `text` 会清掉
HTML，只改文字的调用方（MCP 的 `update_draft`、CLI 的 `draft edit`）都要写一行 `html: None`，读的人还得知道它的
意思是"去掉格式"。

## 决策

- 新类型叫 `DraftBody`（`text: String` + `html: Option<String>`），不叫 issue 里的 `Body`：和 `Draft`、
  `DraftContent`、`DraftChanges` 同一组命名，也不会和收到的信的 `MessageBody` 混淆。
- `DraftContent` 也一起改成 `body: DraftBody`，不只改 `DraftChanges`：否则 `DraftChanges::all` 和 `apply` 还得在
  两种形状之间拆装，约束只修了一半。issue 担心会动到 CLI 的 JSON 和 MCP 的 `DraftOut`，但两者都是逐字段映射
  （`JsonDraft`、`DraftOut`），没有直接序列化 `Draft`，所以对外的格式不变。
- 数据库仍是 `body_text`、`body_html` 两列，只在 store 读写时组装。
- 编辑器这条路也用 `DraftBody`：`Rich::new` 接收、`Rich::written` 返回 `DraftBody`，composer 的 `Fields` 持有它，
  直接放进 `DraftContent`。原来在 gui 里拆成 `(text, html)`、到 frontend 再拼回去，每次自动保存都多复制一遍正文。
- proto 的 `Outgoing` 保持借用的 `text`/`html`：它是组装 MIME 的入参，不是更新草稿的接口。`write_above`、
  `without_nul` 也不动：回复和转发的正文目前总是纯文本，#303 给回复加 HTML 引用时再一起改。

## 方案

- `catmail-types/src/draft.rs`：`DraftBody`、`DraftBody::plain`、`is_formatted`；`DraftChanges::body` 取代
  `text` + `html`，`apply` 回到普通的整体替换。单元测试改为 `a_plain_body_takes_the_formatting_off`。
- 调用方：MCP 的 `create_draft`/`update_draft` 和 CLI 的 `draft edit` 用 `DraftBody::plain`；编辑器（`Rich`、`Fields`）整个传 `DraftBody`；store、core
  （回复、转发、去 NUL、发信组装、删图片）、frontend、gui、CLI/MCP 输出改读 `content.body`。
- `docs/architecture/sending.md` 相应更新。
- 纯重构，不改行为：`cargo fmt`、`clippy -D warnings`、全部测试通过。
