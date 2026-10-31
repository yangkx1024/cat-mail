# 对话框统一用 FadeSlide 进出

- 日期：2026-10-31
- 类型：功能

## 背景

对话框沿用 gpui-kit 默认的 `DialogEntrance::SlideDown`：从窗口顶部拉下来并淡入，关闭时看起来没有动画。
实测（`reduce_motion` 关闭的 GUI 测试）退出其实在播：倒放入场，用同一条先快后慢的曲线，居中的对话框
40ms 走完一半、80ms 已贴到顶边，余下时间只是淡完，所以像是直接消失。

## 决策

- 想要的是缩放加渐变、带 overshoot 回弹。GPUI 没有元素级变换（`TransformationMatrix` 只用于 SVG
  和图片，quad、文字、阴影、裁剪都不认），gpui-kit 的对话框还把曲线截在 0–1；做缩放要 fork gpui-fast
  和 gpui-kit，成本高、长期要 rebase，放弃。`with_rem_size` 假缩放只能缩 rem 尺寸、每帧重排，也放弃。
- 用 gpui-kit 已有的 `FadeSlide`：从终点上方几像素淡入落定，关闭原路返回，180ms（主题 motion token）。
- 菜单（阅读页右键菜单无动画，Select 下拉只有入场）没有给应用的接口，统一要改 gpui-kit，这次不动。

## 方案

- `layout::open_dialog` 包一层 `window.open_dialog`，先设好 `DIALOG_ENTRANCE`；设置、添加发件地址、
  文件夹选择都改走它，`confirm_danger` 的 `AlertDialog` 也设同一个 entrance。
- 测试 `dialogs_settle_in_place_and_take_their_time_to_leave`：开启动画，确认框首帧离终点不超过 8px，
  关闭 90ms 后仍在且只移了几像素，500ms 后卸载。改回 `SlideDown` 时首帧在窗口顶部（15.5px 对 335.5px），测试失败。
- `docs/architecture/gui.md` 的升级须知记下动画的限制。
