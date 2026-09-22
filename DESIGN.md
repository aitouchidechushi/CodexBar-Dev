# Design

## Source of truth

- Status: Active；用户已于 2026-08-30 确认可靠性修复书面规格。
- Last refreshed: 2026-09-22（并发检测呈现增补；原可靠性设计保留）
- Primary product surfaces: Windows 主窗口、托盘面板、托盘悬停窗口、FloatBar、悬浮额度窗口、设置窗口、关于与诊断界面、安装与升级流程。
- Evidence reviewed: `README.md`、`README.zh-CN.md`、`docs/images/tray-panel.png`、`docs/images/settings-providers.png`、`apps/desktop-tauri/src/styles.css`、`apps/desktop-tauri/src/surfaces/`、`apps/desktop-tauri/src/floatbar/`、`apps/desktop-tauri/src/floating-quota/`、`apps/desktop-tauri/src/hooks/useProviders.ts`、Tauri/Rust 事件与持久化代码，以及 `docs/superpowers/specs/2026-08-30-codexbar-reliability-stabilization-design.md`。

## Brand

- Personality: 安静、准确、克制的 Windows 常驻工具；比“炫技”更重视可信度、可追溯性和不打扰。
- Trust signals: 额度数据明确标注新鲜度与来源；版本、构建和运行文件身份可见；保存失败不假装成功；诊断不包含秘密且只由用户主动导出。
- Avoid: 用空白冒充“没有配置”、把瞬时网络错误冒充“额度已失效”、猜测账号关联、无提示覆盖旧数据、自动上传日志、含糊的“最新版”文案、让用户依赖文件名判断版本。

## Product goals

- Goals: 让同一正式发行包在不同 Windows 电脑上表现一致；旧进程和旧配置不能接管或破坏新版本；API Key 与设置在正常退出、崩溃、重启和升级后仍可恢复；瞬时额度失败保留最后一次有效数据并显示真实状态。
- Non-goals: 本轮不重新设计产品视觉、不新增供应商、不猜测供应商未返回的额度、不建立远程遥测或后台日志上传服务。
- Success signals: 用户能够在关于/诊断中确认当前 EXE 路径、哈希、版本和构建；所有已知旧格式均可无损迁移；P0/P1 已知缺陷清零；升级、故障注入和重复全量测试全部通过后才产生正式包。

## Personas and jobs

- Primary personas: 在自己电脑长期运行 CodexBar 的个人用户；需要把同一个正式安装包交给多名非技术用户的维护者；使用多个供应商和多个 API Key 的重度用户。
- User jobs: 快速查看可信额度；添加凭据后确信它已持久化；重启或升级后继续使用；遇到网络波动时区分“数据暂时过期”和“凭据确实失效”；向维护者导出不含秘密的本地诊断。
- Key contexts of use: Windows 10/11，托盘常驻，窗口经常隐藏而非退出，可能存在旧安装版、便携版或调试版残留，网络与供应商接口可能短暂失败。

## Information architecture

- Primary navigation: 保持现有设置顶部导航和供应商侧栏；不因可靠性修复增加新的一级导航。
- Core routes/screens: 托盘/主面板显示额度；设置管理供应商与凭据；关于页显示正式构建身份；诊断导出入口放在关于或高级设置中；启动迁移异常使用阻塞式恢复界面。
- Content hierarchy: 当前可用额度优先，其次是数据更新时间与健康状态，再次是可执行的修复动作；技术细节只在展开项或诊断导出中出现。

## Design principles

- Principle 1: 数据与健康状态分离。最后一次有效额度不能被本次失败覆盖；“旧数据”和“错误原因”必须同时可见。
- Principle 2: 失败必须显式且可恢复。读取、迁移或保存失败时禁止回退成空数据后继续覆盖。
- Principle 3: 单一正式身份。界面和诊断始终显示实际运行文件，而不是只显示语义版本。
- Principle 4: 最小惊扰。正常刷新保持安静；只有需要用户处理的凭据、迁移或进程冲突才使用醒目提示。
- Tradeoffs: 为避免旧版本继续影响新版本，首次稳定化发行优先安装版；便携版只有在完成同等升级与进程接管矩阵后才恢复公开分发。

## Visual language

- Color: 复用 `apps/desktop-tauri/src/styles.css` 的深浅主题、蓝色强调色、绿色成功、橙色过期/降级和红色错误令牌；不能只靠颜色区分状态。
- Typography: 复用现有 SF/Segoe UI/system 字体栈和紧凑字号；构建身份、文件路径和哈希使用可复制的等宽样式。
- Spacing/layout rhythm: 保持现有托盘 280px、弹出面板 340px 和设置双栏节奏；错误说明允许换行，不能为了维持单行而截断关键动作。
- Shape/radius/elevation: 延续现有低对比边框、轻量圆角与面板层级；不新增新的视觉体系。
- Motion: 刷新状态使用轻量、有限动画；尊重 reduced-motion；失败或迁移状态不得闪烁。
- Imagery/iconography: 继续使用现有供应商图标和系统状态图标；版本/迁移/错误图标必须配文字。

## Components

- Existing components to reuse: 供应商卡片、额度条、设置区块、状态徽标、关于页信息行、现有按钮和错误提示样式。
- New/changed components: `BuildIdentityPanel`（或现有关于页等价区块）、`DataHealthBadge`、`MigrationRecoveryPanel`、`DiagnosticExportAction`；所有额度表面统一消费同一个“最后有效数据 + 健康状态”视图模型。
- Variants and states: fresh、refreshing、stale-with-error、auth-required、never-loaded、migration-blocked、storage-write-failed、superseded；状态名称由共享类型定义，不能各窗口自行推断。
- Token/component ownership: 全局颜色/排版仍由 `styles.css` 管理；供应商数据状态由共享 Rust/TypeScript bridge 类型管理；各表面只负责呈现。

## Accessibility

- Target standard: 以 WCAG 2.2 AA 的可读性、键盘操作和状态表达为目标，并遵守 Windows 桌面交互习惯。
- Keyboard/focus behavior: 新增按钮和恢复动作可通过 Tab 到达，使用现有 focus ring；阻塞式迁移界面必须有明确的首个焦点和退出路径。
- Contrast/readability: 成功、降级和错误同时使用文字/图标/颜色；小字号状态文案仍需达到可读对比。
- Screen-reader semantics: 刷新完成、降级和保存失败使用合适的 live region，但避免每次后台轮询都打扰；进度条保留可读名称和值。
- Reduced motion and sensory considerations: reduced-motion 下停用循环动画；不使用仅依赖闪烁或颜色的提示。

## Responsive behavior

- Supported breakpoints/devices: Windows 10/11 的 WebView2；托盘、悬停、FloatBar、悬浮额度和设置窗口分别保持其当前受控尺寸。
- Layout adaptations: 窄表面显示短状态和更新时间，详细原因放入 tooltip/展开区；设置窗口显示完整恢复动作和构建身份。
- Touch/hover differences: 核心动作不能只存在于 hover；tooltip 只是补充，不能承载唯一错误说明。

## Interaction states

- Loading: 保留现有最后有效数据并标注“正在刷新”；首次无数据才显示骨架/等待状态。
- Empty: 仅在存储被确认不存在且迁移已完成时显示“尚未添加”；读取失败、解密失败、格式不支持不能显示为空。
- Error: 瞬时供应商错误显示降级状态并保留数据；认证错误显示需要处理；本地存储错误阻止可能覆盖原文件的写操作。
- Success: 保存成功必须以已写入并回读验证后的状态为准；刷新成功更新数据时间和健康状态。
- Disabled: 禁用供应商后立即停止/取消对应刷新，清理该供应商显示，但不删除凭据。
- Offline/slow network, if applicable: 使用最后有效数据、明确时间、有限退避和手动重试；不得把网络离线显示成“未添加 API Key”。

## Content voice

- Tone: 直白、具体、可执行，不用模糊的“发生错误”或“最新版”。
- Terminology: 区分“程序版本”“构建标识”“运行文件”“最后有效额度”“本次刷新”“凭据失效”“存储无法读取”。
- Microcopy rules: 每条严重提示回答三件事：发生了什么、数据是否仍安全、用户现在能做什么；永不展示原始 Key、Cookie、Token 或验证码。

## Implementation constraints

- Framework/styling system: Tauri + Rust + React，沿用仓库现有 CSS 变量和 bridge 类型；不引入第二套状态管理或设计系统。
- Design-token constraints: 新状态优先复用 `--provider-status-*`、`--text-*`、`--focus-ring` 等令牌；确需新增时同时提供深色和浅色值。
- Performance constraints: 托盘常驻刷新不能阻塞 UI；诊断哈希可缓存但必须与当前进程映像一致；重试必须限流。
- Compatibility constraints: 支持 Windows 10/11、旧 0.45.2 数据和已公开的旧安装/便携场景；新存储必须隔离旧写入器。
- Test/screenshot expectations: 所有新增状态有 Rust/TypeScript 单元测试；关键表面有组件测试；发布前在真实 Windows 上完成安装、升级、托盘隐藏、旧便携版和重启矩阵。

## Open questions

## 并发检测结果呈现（2026-09-22 用户确认）

- 统一按钮发起检测；不再在窗口顶部堆放完整结果。
- 每个供应商在额度读取失败计数旁显示“并发检测结果”；没有失败时使用同一位置，仅有该供应商检测记录时显示入口。
- 悬停入口展示仅属于该供应商的结果浮层；入口与浮层作为共同悬停区域，短暂跨越间距不隐藏，离开两者后隐藏。键盘聚焦可打开、Escape 可关闭。
- 使用固定浮层避免被额度卡片裁切，避让窗口边缘，长结果可滚动；页面滚动或窗口失焦时关闭。
- 使用账号标签、状态、次要模型与时间的层级；明确区分响应重叠、并发受限和未判定。历史受限结果不冒充当前结论。
- 保留 Key 卡片上的受限异常提示，不修改额度统计或检测算法。
- 2026-09-22 后续用户明确要求：移除二次确认表单，点击直接检测，主窗口按钮位于“刷新全部”左侧。保留按钮悬停费用说明、后台凭据范围绑定与取消功能，使用现有默认模型，不再在该流程展示模型编辑表单。

- 当前没有阻塞书面设计评审的问题。代码签名证书属于发行环境依赖：有证书时必须签名；无证书时仍不得省略哈希、构建清单、不可变版本和所有可靠性门禁。
