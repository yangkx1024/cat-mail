# 修复远程图片取消测试的偶发失败

- 日期：2026-10-23
- 类型：修复

## 背景

`main` 上 #77 合并后的 CI（run 39889978785）在 ubuntu 上失败：
`fetches_remote_images_as_they_come_until_dropped` 报 `timed out waiting for the hanging
request to be dropped`。同一份代码在 PR 上通过，属于偶发失败。

测试拿到另外三个结果后就 drop stream，再等 `FakeHttp::abandoned() == 1`。但机器繁忙时，
`/hang` 的 fetch 任务此时可能还没把请求发出去：abort 后服务端根本收不到请求头，进不了
`Reply::Held` 分支，`abandoned` 永远是 0。取消逻辑本身没有问题。

## 决策

只改测试：drop 之前先等服务端收到 `/hang` 的请求头。考虑过让 testkit 把"请求头没读完就断开"
也算作 abandoned，但请求可能根本没发出，那样测试就证明不了"取消了一个进行中的请求"，放弃。

## 方案

`crates/catmail-core/tests/bodies.rs`：drop stream 前用 `eventually` 等 `server.heads()` 里出现
`GET /hang`。

验证：在 `images.rs` 中临时给 `/hang` 的 fetch 加 300ms 延迟（未提交），修复前测试稳定复现同样的
超时，修复后通过；去掉延迟后 fmt、clippy、全量测试通过。
