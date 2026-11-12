# MCP：本机文件经上传附到草稿，不经过模型

- 日期：2026-11-12
- 类型：功能
- Issue：#287

## 背景

agent 要把本机的文件附到草稿上，只能把 `{name, base64}` 写进工具参数：一份 212 KB 的 PDF 是 28 万多字的
base64，得由模型逐字写出，远超一次能可靠写完的长度，写到一半被截断。请求体（草稿层级 16 MB）和单个文件 10 MB
的上限都不是瓶颈，瓶颈是内容要经过模型的输出。读的方向（`read_attachment` 的 blob 由客户端存成文件）没有这个问题。

## 决策

- **上传端点，内容不经过模型**：`prepare_upload`（草稿层级）返回 `{upload, url, expires_at}`，agent 在 shell 里
  `curl -T 文件 地址` 传原始字节，再把 `{name, upload}` 交给 `attach`、`create_draft`、`reply`、`forward`。
  `FileIn` 的 `base64` 与 `upload` 都成了可选，给且只给一个。"工具里没有路径"不变：服务端只收内容，读文件的是
  agent 自己的进程、用它自己的权限。
- **地址就是凭据，上传不要令牌**：issue 原来的设想是 agent 带 Bearer 令牌 `PUT /mcp/uploads`。但令牌在客户端的
  配置里（Claude Code 写在请求头里），模型看不到，shell 里也没有；Codex 的 `CATMAIL_MCP_TOKEN` 在 shell 里能读到
  只是碰巧。所以像预签名 URL 那样，句柄放在地址里（`/mcp/uploads/<32 位十六进制>`），属于要它的令牌，只收一个
  文件，15 分钟内有效。带 `Origin` 的上传同样拒绝；`PUT`、`POST` 都收（`curl -T` 与 `--data-binary`）。
- **服务端拼出完整地址**：模型不知道自己连的是本机还是 `*.ts.net`。主机取 `X-Forwarded-Host`，否则 `Host`，都没有
  或含有主机名以外的字符时用监听的地址；协议取 `X-Forwarded-Proto`，没有时本机名字（`localhost`、回环地址）用
  `http`，别的名字用 `https`（只有代理带来别的名字，`tailscale serve`、Cloudflare Tunnel 都是 HTTPS）。客户端连得到
  的地址，同一台机器上的 shell 也连得到。挂在代理的子路径下时地址里没有那段路径，没处理。
- **调用成功后才用掉**：工具里取上传的内容是复制一份，引擎的调用成功后才删掉用到的上传；失败（文件名不合法、
  草稿在 GUI 里开着）的调用留着它，agent 不必重传。一次调用里同一个句柄给两次是 `invalid`。并发的两个调用用同一个
  句柄会附两次，都是同一个令牌，不另防。
- **内存的上限**：在内存里（仿照 `tools/sending.rs` 的 `Asked`），宿主重启就忘。每个令牌最多 16 个没用掉的上传；
  收到而没用掉的合计不超过 20 MB（两次调用的量，一次失败了还能准备下一次）；一个文件不超过 10 MB（`Limited`，超了
  413，那个上传可以再传）。开始接收时把上传标成接收中，另一个请求传不进来；两个上传同时接收时，收完再按合计查一次。
  每次准备、上传、取用时都清掉过期的，收到而没用掉的内容不会一直留到下一次 `prepare_upload`。
- **句柄不进日志**：Codex 审查指出，没上传就 `attach` 等的错误里原本带着完整的句柄，`call_tool` 把错误原样记进
  info 日志（参数虽然换成了字数）。还在等文件的句柄谁读到日志都能在过期前传进自己的内容，agent 随后就附进草稿。
  现在 `Mail::uploaded` 与"同一个上传给了两次"的错误只写文件名（`name`），agent 照样知道是哪个；测试查错误里没有
  句柄。
- **放弃的方案**：分块追加 base64（内容仍经过模型，十几次调用，慢且费 token）；给工具加路径参数（agent 被注入后能
  把任意文件附进外发的信）。

## 方案

- `catmail-types`：`NewFile::Content` 的内容是 `Bytes`；根 `Cargo.toml` 加 `bytes`。
- `catmail-mcp`：新 `uploads.rs`（`Uploads`：`prepare`、`begin`、`file`、`used`；`begin` 返回 `Receiving`，
  `put` 存下内容，没 `put` 就丢掉时回到可以再传，`too_large` 分出是单个文件太大还是令牌的上传满了；内容是 `Bytes`；
  `MAX_FILE_BYTES` 从 `tools/drafts.rs` 移来）；`lib.rs` 的 `random_handle`，发信确认的 `Asked` 也用它；
  `http.rs` 在查令牌之前分出 `/mcp/uploads/<句柄>`（`upload`、`refused_upload`），`Routes` 把 rmcp 的服务、令牌、
  上传、监听的地址一起交给每个连接，验过令牌的请求另带 `BaseUrl`（`base_url`）；回应的文字只经一个 `text`；
  `tools/drafts.rs` 的 `prepare_upload`、`FileIn`、`Mail::files`（返回 `Given`：文件与用到的上传；上传的内容先查
  合计再复制）；`output.rs` 的 `UploadOut`；`tier_of` 加 `prepare_upload`；快照重新生成。日志的 `logged` 不用改：
  `upload` 是字符串，记成字数。测试里 `try_send` 改成 `send_bytes` 的薄包装。
- `/simplify` 之后：上面的 `Receiving` 守卫（原先是 `begin`、`put`、`abandon` 三步，`http.rs` 要记得在出错时
  `abandon`，请求被中止时漏掉）、`PutRefused::TooLarge`（原先 `http.rs` 按剩余空间是否到 10 MB 猜）、地址移到
  `http.rs`（`Mail` 不再为它带监听的地址）、`Bytes` 免去收到时的一次复制、`MAX_FILE_BYTES` 移到 `uploads.rs`
  （原先 `uploads` 与 `tools` 互相引用）、`random_handle`、两个测试辅助合一。随后把 `catmail-types` 的
  `NewFile::Content` 的内容从 `Vec<u8>` 改成 `bytes::Bytes`（新的工作区依赖，hyper 早已带进来，`cargo deny` 通过）：
  原先每次用到上传的调用都要复制一份（调用失败要留着上传，内容不能移走），现在交给引擎只是加引用计数，从收到到
  写进 blob 不复制。没选 `Arc<[u8]>`：从 hyper 的 `Bytes` 转过去总要复制一次。引擎只借用内容（`store_content`
  收 `&[u8]`），core 只有测试跟着改。留下没做的：四个草稿工具各自"取文件、成功后 `used`"没收进一个辅助，它们
  取文件的时机各不相同。
- 文档：`mcp.md` 的工具表、传输一节的例外、"工具里没有路径"、新的"附件经上传传入"、代码一节。
- 验证：`uploads.rs` 的单元测试（只收一个文件、别的令牌拿不到、过期、用过即删、接收失败可重传、合计与个数的上限、
  `http.rs` 的 `the_base_url_is_the_address_the_client_used`：地址按 `Host`、`X-Forwarded-*` 拼且不收奇怪的主机名）；经 HTTP 的 `files_put_to_an_upload_go_into_drafts`：
  地址是请求来的 `localhost:端口`，没传就用是 `invalid`，带 `Origin` 403、`GET` 405、编的句柄 404，不带令牌上传
  300 KB 成功、再传 409，失败的调用（坏名字、同时给 base64、同一句柄两次）留着上传，之后建草稿成功（类型、大小），
  用过后再用 `not_found`、再传 404；别的令牌的上传这个令牌用不了、它自己能用；超过 10 MB 被拒后同一个上传还能传。
  没有 Nix，用系统的 cargo。没有经 `tailscale serve` 实际试过 `X-Forwarded-Proto` 是否带上（不带时按名字猜 `https`）。
