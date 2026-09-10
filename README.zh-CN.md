# Win-CodexBar

## 中文目录导航

查看[仓库完整中文目录表](README.md#中文导航下载程序与理解目录)，或直接打开：[桌面界面与窗口控制](apps/desktop-tauri/README.md)、[核心业务与服务商适配](rust/README.md)、[项目文档](docs/README.md)、[构建发布脚本](scripts/README.md)、[GitHub 协作配置](.github/README.md)。

**下载可运行程序请进入 [Releases](https://github.com/aitouchidechushi/CodexBar-Dev/releases/tag/user-confirmed-0.45.2-20260825)，选择 Assets 中的 `.exe`。** 当前开发源码与该历史程序不是同一次构建。文件列表中间一列是最近一次提交说明，不是固定的目录介绍。

> **本仓库上传说明：** main 保存当前 0.46.0 / build 87 开发进度；单独分享的 0.45.2 是所有者确认正在使用的历史 Debug 程序，并非由当前源码构建。下载使用和朋友继续开发请先看[协作说明](PUBLICATION.md)。下文原有稳定版发布规范不代表本开发快照已完成稳定版验收。

[English](README.md) | [简体中文](README.zh-CN.md)

Win-CodexBar 是一个 Windows 桌面 AI 编程服务额度监控工具。它可以在主窗口、系统托盘相关窗口、FloatBar 和悬浮额度窗口中显示服务商用量、重置时间、账号级月额度，以及由程序分别管理的多个 API Key。

项目使用 Tauri、React 和 Rust 构建，设计来源于原版 macOS [CodexBar](https://github.com/steipete/CodexBar)。

<table>
  <tr>
    <td align="center">
      <img src="docs/images/tray-panel.png" width="320" alt="Win-CodexBar 托盘面板与服务商额度"/>
      <br/>
      托盘面板
    </td>
    <td align="center">
      <img src="docs/images/settings-providers.png" width="520" alt="Win-CodexBar 服务商设置"/>
      <br/>
      服务商设置
    </td>
  </tr>
</table>

## 核心功能

- 显示各服务商实际返回的额度和用量，包括会话或 5 小时额度、7 天额度、真实月额度、余额及重置时间。
- 同一服务商可以添加多个凭据，每个 API Key 独立显示；单个凭据获取失败不会隐藏或覆盖其他凭据。
- 主窗口、托盘悬停窗口、FloatBar 和悬浮额度窗口使用一致的额度分类与账号归属关系。
- 月额度使用明确不同的视觉样式，并与已经确认属于同一账号的一个或多个 API Key 分组显示。
- 没有确认账号归属关系的 API Key 继续作为独立条目显示，程序不会猜测关联关系。
- 按程序现有的刷新周期在后台更新用量。
- 提供中文、英文、日文、韩文和西班牙文界面。

不同服务商能够提供的数据并不相同。Win-CodexBar 只显示服务商真实返回的额度周期和余额，不会把 7 天额度、账单金额或 MCP 额度改名成模型月额度。

## 通过浏览器已有登录状态获取 Kimi 月额度

Win-CodexBar 可以读取 Microsoft Edge 和 Google Chrome 中已经登录的 Kimi 账号，并显示这些账号的月额度。

1. 开启该功能，并完成首次只读访问授权。
2. Win-CodexBar 读取已登录 Kimi 账号的身份和月额度。无需安装浏览器扩展；浏览器登录状态仍有效时，也无需再次登录 Kimi。
3. 程序不会控制或刷新浏览器页面。后续刷新额度所需的最新续期 Token 使用 Windows 凭据管理器保护。
4. 每个 Kimi 账号只保存和显示一份月额度；只有身份信息能够确认归属关系时，才会把该月额度与对应的一个或多个 Kimi API Key 分组。
5. 每个已关联 API Key 仍然分别保留自己的 5 小时和 7 天额度；没有关联的 Key 会单独显示并明确标记。

支持 Edge 和 Chrome 的多个配置文件以及多个 Kimi 账号。退出 Win-CodexBar 或重启 Windows 后，通常不需要重新授权。如果 Kimi 登录状态或续期 Token 已失效，程序会提示账号需要处理，不会把月额度静默关联到其他 API Key。

现有的 **登录新的 Kimi 账号** 功能仍然保留，用户也可以选择在 Win-CodexBar 的独立 Kimi 官方窗口中登录。通过浏览器获取月额度不会删除或改变原有登录方式。

## 额度显示说明

| 服务商或额度类型 | 显示规则 |
| --- | --- |
| Codex | 会话额度、7 天额度，以及账号能够提供的余额或相关数据 |
| Claude | 账号实际返回的 5 小时和 7 天额度 |
| GLM（智谱 BigModel / z.ai） | API Key 的模型额度显示 Rate Limit（5 小时）和 Weekly（7 天）；单独的 MCP 月额度不会作为模型 API 月额度显示 |
| Kimi API Key | 每个 Key 分别显示自己的 5 小时和 7 天额度 |
| Kimi 浏览器账号 | 显示账号级月额度，并只与身份确认一致的 Kimi API Key 分组 |
| 其他存在真实月上限的服务商 | 上游返回有限月周期且账号身份稳定时，按相同的月额度策略显示 |

月额度临时刷新失败时，程序保留上一次有效结果并显示错误状态，不会覆盖各 API Key 独立的 5 小时或 7 天额度。

## 支持的服务商

当前服务商注册表包含 **62 个有效服务商**：

<details>
<summary>展开完整服务商列表</summary>

Codex、Claude、Cursor、Factory、Gemini、Antigravity、Copilot、GLM（智谱 BigModel / z.ai）、MiniMax、Kiro、Vertex AI、Augment、OpenCode、Kimi、Amp、Warp、Ollama、Azure OpenAI、T3 Chat、OpenRouter、JetBrains AI、Alibaba、Alibaba Token Plan、NanoGPT、Infini、Perplexity、Abacus AI、Mistral、OpenCode Go、Kilo、AWS Bedrock、Codebuff、DeepSeek、DeepInfra、ai&、Windsurf、Manus、Xiaomi MiMo、Doubao、Command Code、Crof、StepFun、Venice、OpenAI API、Grok、ElevenLabs、Deepgram、Groq、LLM Proxy、Chutes、LiteLLM、Poe、Devin、Zed、Qoder、Sakana AI、sub2api、Wayfinder、ZenMux、ClinePass、LongCat、Neuralwatt。

</details>

认证方式和可显示的数据取决于服务商、账号类型、套餐、地区及上游接口。旧配置仍可解析 Kimi K2 和 CrossModel 标识，但它们不作为当前有效支持的服务商展示。

## 下载稳定版

公开发布只提供稳定的 Windows 版本。请打开当前仓库的 **Releases** 页面，并按需要下载其中一个文件：

- \`CodexBar-<version>-Setup.exe\`：适合普通 Windows 用户的安装程序。
- \`CodexBar-<version>-portable.exe\`：无需安装的便携程序。

每个程序文件同时提供一个对应的 \`.sha256\` 校验文件。可以在 PowerShell 中校验下载文件：

\`\`\`powershell
Get-FileHash -Algorithm SHA256 .\CodexBar-<version>-Setup.exe
\`\`\`

每个稳定版本只应包含一份权威安装程序、一份权威便携程序，以及两个对应的哈希文件。Debug 程序、重复程序、旧的重复构建、测试输出和本地验收材料都不作为 Release 附件。历史稳定版本可以保留，但只有最新稳定版标记为 **Latest**。

关于页面可以提示存在新的稳定版本，但不会静默下载，也不会强制用户安装。

## 首次使用

1. 启动安装版或便携版程序。
2. 打开 **设置 → 服务商**，启用需要监控的服务商。
3. 按不同服务商的要求添加或导入凭据。
4. 需要分别查看多个 API Key 时，将每个 Key 单独添加。
5. 需要显示 Kimi 账号月额度时，打开 **设置 → Kimi 账号**，选择读取浏览器账号，或使用可选的程序内 Kimi 登录方式。

## 隐私与本地数据

- 除了向对应服务商请求用量所需的网络访问外，用量数据和凭据保留在本机。
- 由程序管理的 API Key、手动 Cookie 和账号 Token 使用程序的本地安全凭据存储保护。
- Kimi 浏览器账号的续期 Token 使用 Windows 凭据管理器保护。
- 读取浏览器账号是只读操作，并且只会在用户授权后开始。
- 原始 API Key、Cookie、Bearer Token、OAuth Token、密码和验证码不得出现在诊断信息或公开发布文件中。
- Win-CodexBar 不会为了刷新 Kimi 月额度而控制或刷新 Edge、Chrome 页面。

## 从源码构建

环境要求：

- Windows 10 或 Windows 11
- Rust stable，并安装 \`x86_64-pc-windows-msvc\` target
- Microsoft Visual Studio Build Tools，并安装 **Desktop development with C++**
- Node.js 20+ 和 pnpm 10

\`\`\`powershell
cd apps\desktop-tauri
pnpm install --frozen-lockfile
pnpm run tauri:build
\`\`\`

Release 可执行文件输出到 \`target\release\codexbar-desktop-tauri.exe\`。直接构建并不等于完成公开发布；发布前还需要制作安装程序、生成哈希、从干净源码状态核验并进行安装冒烟测试。

更多信息请查看[源码构建](docs/BUILDING.md)、[Windows 说明](docs/WSL.md)、[Cookie 处理](docs/COOKIES.md)和[发布检查](docs/release/ci-cd.md)。

## 支持的语言

- 英语
- 简体中文
- 繁体中文
- 日语
- 韩语
- 西班牙语（墨西哥）

## 致谢

- 原版 macOS 项目：[steipete/CodexBar](https://github.com/steipete/CodexBar)，作者 Peter Steinberger
- Windows 桌面移植和服务商适配：Win-CodexBar 贡献者

## 许可证

本项目使用 [MIT License](LICENSE)。
