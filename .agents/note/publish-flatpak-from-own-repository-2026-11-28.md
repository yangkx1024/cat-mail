# Flatpak 发布到自己的仓库，自动更新

- 日期：2026-11-28
- 类型：功能

## 背景

Linux 版只在 release 上发一个不带仓库地址的 `.flatpak` bundle，装上后永远停在那个版本。原计划上 Flathub，但
它的生成式 AI 政策要求 manifest 里没有 AI 生成或辅助的内容，投递 PR 也不能由 AI 开或回复，决定不上（#406）。

## 决策

设计和理由写在 `docs/architecture/packaging.md` 的"仓库与更新"和"发布"里，这里只记取舍：

- 自己托管在 R2（`catmail.yangkx.net/flatpak/`），不用 GitHub release：OSTree 仓库是大量小文件。
- 分支 `stable`；单独一把永不过期的仓库密钥，不用个人密钥。
- 增量覆盖最近三个版本。实测 `build-update-repo --generate-static-deltas` 只出从父提交的增量，还删掉它没生成的，
  所以另外两个在它之后用 ostree 补。
- 签名放在发布时单独的 job 里，不在构建 job 里：那样 PR 的构建也要拿到私钥。实测带 `--gpg-keys` 的 bundle
  要求提交已签名，所以发布的 bundle 在这个 job 里从签好名的仓库打。
- 先更新仓库、再创建 release：仓库才是用户拿到的东西。没有把它拆成"准备"和"上传"两个 job 来和 macOS 的构建
  并行（每次发版能省几分钟），两个 job 之间要搬整个仓库，不值得。
- AppStream 的 `<releases>` 由 `version` job 写进 git，不在构建时注入。

## 方案

- `packaging/linux/update-flatpak-repo.sh`（仓库这边的全部工作，`--check` 检查发布出去的仓库，`--test-key`
  用临时密钥试跑）、`manifest.sh`（两个脚本共用的 id、分支、仓库文件）、公钥和 `.flatpakrepo`/`.flatpakref`；
  manifest 加 `branch: stable`。
- `.github/workflows/release.yml`：`version` job 写 `<releases>`；新 `flatpak-repo` job；release 只取 `dist-*`
  artifact。
- `tests/packaging.rs` 核对两个新文件的地址、分支、公钥。
- 验证：本地用临时密钥，从真实构建的 bundle 派生出 v2–v5，逐个发布进本地仓库，经本地 HTTP 服务器观察请求：
  从签名的 bundle 装上后，v1→v4 只取了一个静态增量；v5 之后 v1 被修剪，v4、v3、v2 各有增量；同一版本重跑不加
  提交；已被修剪的 v1 逐个下载对象也能升到 v5。应用内的更新提示是 #406 第 4 步，另开 PR。
