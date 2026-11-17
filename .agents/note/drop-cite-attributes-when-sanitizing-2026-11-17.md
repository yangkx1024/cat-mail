# 清洗时去掉 `cite` 属性

- 日期：2026-11-17
- 类型：修复

## 背景

审计 #331：ammonia 缺省在 `blockquote`、`q`、`del`、`ins` 上保留 `cite`，不检查 scheme；清洗只把它当 URL 属性
拦 `data:`，所以 `<blockquote cite="javascript:x">` 原样留下，与 bodies.md 说的“链接只允许 http、https、mailto、
cid”不符。

## 决策

整个去掉 `cite`（`rm_tag_attributes`），而不是查 scheme：浏览器既不显示也不跳转它，留着没有用处。catmail 自己写信
时用的 `type="cite"` 是另一个属性，不受影响（收到的信里 `type` 本来就被清洗去掉）。清洗收紧，按 bodies.md 的规定
把 `CONTENT_VERSION` 升到 6，已存的正文重建；指纹语料里没有 `cite`，哈希不变。

## 方案

`catmail-html` 的 `sanitize.rs` 加 `rm_tag_attributes` 并加测试 `a_quote_s_source_url_is_removed`；
`catmail-proto` 的 `CONTENT_VERSION` 升到 6（指纹测试同步）；`docs/architecture/bodies.md` 写明。
