# 应用图标

暖白信封上盖一枚橘色猫脚印，信封略微倾斜，外层为圆角底板。三份 SVG 的画布均为
512 × 512，可无损缩放，无字体、脚本、外部资源或位图依赖。

- `catmail.svg`：默认亮色，通过 `prefers-color-scheme: dark` 自动切换暗色。
- `catmail-light.svg`：固定亮色，所有颜色直接写入 SVG 属性。
- `catmail-dark.svg`：固定暗色，所有颜色直接写入 SVG 属性。
- `catmail-menu-bar.svg`：macOS 状态栏用的单色模板图，只有信封和猫爪、不倾斜，没有底板；系统只看
  alpha，按菜单栏着色。

浏览器可使用自动版；桌面图标加载器通常不响应 CSS 媒体查询，应根据主题选择固定版本。
AppImage 用 `catmail-light.svg` 作桌面图标（见 `docs/architecture/packaging.md`）。
