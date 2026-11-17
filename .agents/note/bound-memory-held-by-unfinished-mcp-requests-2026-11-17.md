# MCP：没发完的请求占着的内存有上限（上传预留、读超时、每个令牌同时在答的请求数）

- 日期：2026-11-17
- 类型：修复
- Issue：#324

## 背景

`uploads::room()` 只统计已收到的文件，`begin()` 给每个同时开始的 PUT 都是 `min(剩余, 10 MB)`；`Limited::collect()`
没有超时；`live()` 按准备时刻清过期条目，正在接收的也清掉，令牌 16 个名额随之空出。于是一个草稿层级令牌准备
16 个句柄、开 16 个各发不到 10 MB 就停住的 PUT，就占 160 MB；15 分钟后条目过期，再来一轮，越叠越多。与 `mcp.md`
写的「合计不超过 20 MB」不符。原来的测试 `a_token_holds_only_so_much` 断言了同时开始的每个都可以是满额。

## 决策

- **按最多能长到的大小预留**：`State::Receiving { reserved }`，`room()` 把它算进去。issue 建议一律留 `limit`，
  但那样 agent 并行 `curl -T` 三个小文件时第三个会被拒。`curl -T` 带 `Content-Length`，所以有声明的长度就只留它
  （超出剩余空间或 10 MB 的不读就 413）；没有（chunked）才留 `min(剩余, 10 MB)`。预留之后 `put` 不必再按合计查一次。
- **超时到上传过期为止，而不是一两分钟**：issue 建议包一两分钟的超时，但第 14:59 开始的 PUT 仍会在条目过期后
  多跑那么久；慢的网络传 10 MB 也可能超过两分钟。内存已由预留限住，超时只是收回名额，所以期限就是上传本身的
  过期时刻（`Receiving::until`），超了回 408。
- **正在接收的条目不随过期清掉**：`live()` 留着 `Receiving` 的条目直到守卫结束，名额和预留在请求体还在缓冲时
  不会再给出去；守卫结束回到 `Waiting`，下一次使用就清掉。`put` 不再另查过期：读请求体的超时已经截在过期时刻，
  恰好在那之前收完的照收，下一次使用清掉，过期只在 `live()` 与超时两处管。

## 方案

- `crates/catmail-mcp/src/uploads.rs`：`begin(handle, declared, now)`，`Receiving` 带 `limit`（预留）与 `until`；
  `room()`、`live()` 如上。`too_large` 照旧按 `limit` 判断：有声明长度时请求体不会超出，只有 chunked 的会走到它。
- `crates/catmail-mcp/src/http.rs`：用请求体的 `size_hint().exact()`（hyper 按 `Content-Length` 给）作声明的长度；
  `tokio::time::timeout_at(until, collect)`，超时 408；`Full` 的提示提到正在接收的上传。
- 文档：`docs/architecture/mcp.md` 的「在内存里，用过即删」。
- 验证：`uploads.rs` 改写 `a_token_holds_only_so_much_with_the_files_being_put`（同时开始的两个占满 20 MB、第三个
  `Full { room: 0 }`，结束的一个还回预留，声明长度只留那么多，超过剩余或 10 MB 的直接拒），新增
  `a_file_being_put_holds_its_slot_and_room_past_the_expiry`；经 HTTP 的 `files_being_put_hold_their_room_until_they_end`
  （两个 chunked、`Expect: 100-continue` 停住的 PUT 让 1 字节的上传 413，断开一个后成功）。把 `room()`、`live()`
  改回原样时这三个测试都失败。没有 Nix，用系统的 cargo。408 的超时没有测试（要等 15 分钟或注入时钟）。
- `/simplify` 之后：去掉 `Receiving::room` 与 `put` 的过期检查（见上）；"发出请求头、等到 `100 Continue`"收进
  测试的 `awaiting_body`，`stopping_cuts_off_a_request_whose_body_never_ends` 也用它；重试的循环每次等 10 ms。
  留下没做的：让 `Receiving` 自己读请求体（`receive(body)`，超时由守卫拥有，`limit`、`until` 不再公开），只有
  `http.rs` 一个调用者，暂不值得把 hyper 的请求体搬进 `uploads.rs`。

## `/mcp` 的同类问题

审查时发现 `/mcp` 本身也一样：验过令牌后请求交给 rmcp 读请求体，没有读超时；调用频率在工具调用时才数，停住的
请求走不到那一步；一个令牌同时开着多少个请求也不限。草稿层级的令牌开任意多个声明 16 MB 的 POST、各发一部分就
停住，内存没有上限，比 #324 还宽。一起修：

- **读超时按常规 HTTP 服务**：给 hyper 的连接 tokio 的定时器，请求头的 30 秒超时（hyper 的缺省值，没有定时器时
  不生效，原先就是没生效）才起作用；请求体包一层 `ReadTimeout`，60 秒没有新内容就出错（nginx 的
  `client_body_timeout`），算两段之间而不是整个，慢的网络传 16 MB 照样传得完。在 `respond` 一开头包，上传也用它
  （出错是 408，可以再传），上传仍另有到过期为止的期限。
- **每个令牌同时在答的请求最多 16 个**：`InFlight` 按令牌名计数，验过令牌、交给 rmcp 之前进，守卫 `Answering`
  在答复做完或请求被中止时退。rmcp 用 `json_response`，`handle` 返回时调用已经做完，守卫跨过它就数准了。超了答
  429：这时还没读请求体，没有 JSON-RPC 的 id，没法像调用频率那样作为工具的错误答。只读超时不够：一点一点地发
  就能一直占着，所以要限个数，一个令牌最多 16 × 16 MB。上传不另算进来：一个令牌最多准备 16 个上传，每个同时只收
  一个文件。考虑过让第 17 个排队而不是拒绝（不读它的请求体就不占内存），但那样要另管排队的连接，正常的 agent 也
  不会同时开 16 个调用，就按拒绝做。
- 涉及：`crates/catmail-mcp/src/http.rs`（`HEADER_READ_TIMEOUT`、`BODY_READ_TIMEOUT`、`REQUESTS_PER_TOKEN`、
  `ReadTimeout`、`BodyTimedOut`、`InFlight`、`Answering`）；`Cargo.toml` 的 dev-dependency 给 tokio 加 `test-util`
  （暂停的时钟）；`mcp.md` 的传输一节与限额一节。
- 验证：`a_body_times_out_only_once_none_of_it_arrives_for_long`（暂停的时钟，三段各隔 59 秒的请求体收得完，
  停住的 60 秒后 `BodyTimedOut`）；`InFlight` 的单元测试；经 HTTP 的
  `tests::server::a_token_has_only_so_many_requests_answered_at_once`（16 个停住的 POST 后第 17 个 429，别的令牌
  不受影响，断开一个后又能答；把上限改成 1000 时失败）。请求头的 30 秒超时没有测试（是 hyper 的行为，要等 30 秒）。
