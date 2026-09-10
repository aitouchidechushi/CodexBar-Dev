# 桌面应用：界面与窗口控制

本目录是用户实际操作的桌面应用，使用 React、TypeScript、Tauri 和 Rust。它调用仓库根目录下 `rust/` 提供的核心业务能力。

| 路径 | 中文说明 |
| --- | --- |
| `src/components/` | 额度卡片等可复用界面组件。 |
| `src/surfaces/` | 主面板、设置页等界面。 |
| `src/floatbar/`、`src/floating-ball/`、`src/floating-quota/` | 浮动条、悬浮球与悬浮额度界面。 |
| `src/tray-hover/` | 鼠标悬停托盘图标时显示的界面。 |
| `src/hooks/`、`src/lib/` | 界面状态、数据更新和后台调用辅助逻辑。 |
| `src/types/`、`src/i18n/` | 数据类型定义与界面多语言支持。 |
| `src-tauri/src/commands/` | 接收前端调用的后台操作。 |
| `src-tauri/src/shell/` | 桌面窗口创建、定位和管理。 |
| `src-tauri/src/kimi_accounts/` | Kimi 账号相关桌面逻辑。 |
| `src-tauri/capabilities/` | Tauri 权限配置。 |
| `src-tauri/tauri.conf.json` | 应用、窗口、构建和打包配置。 |
| `package.json` | 前端依赖以及启动、测试、构建命令。 |
| `pnpm-lock.yaml` | 前端依赖版本锁定文件，需要提交到仓库。 |

改界面优先查看 `src/`；改窗口、托盘和桌面生命周期优先查看 `src-tauri/src/`；改服务商额度接口优先查看根目录 `rust/src/providers/`。

环境准备、运行和测试步骤见[使用与协作说明](../../PUBLICATION.md)。下载即可运行的历史程序在 GitHub Releases，不在本目录。
