# CodexBar-Dev：使用与协作说明

## 当前程序与历史版本

当前开发源码版本为 **0.47.0 / build 88**。本次只提交源码、测试与协作说明，不发布新的 Release，也不启用更新源。下方 0.46.0 链接仍为此前发布的下载包，并非当前源码重新构建的程序。

0.47.0 包含 Kimi 多浏览器刷新结果保护、临时失败时的缓存状态修正、共享额度选择、安装版 CLI 启动状态查询，以及新安装包的覆盖保护和卸载启动项清理。自动测试记录：Rust 1,483 项、前端 346 项、安装逻辑隔离测试 47 项通过；用户反馈本地便携验收版当前可以正常启动和使用。这不代表所有功能均已验收。

已知未完成项：便携版重复启动仍可能被启动保护拦截；更新检查尚未区分部分请求失败与无新版；真实安装/升级/卸载、历史配置跨机器迁移和旧新进程交接仍需验证。已分发的旧安装包不会自动获得新保护。更新源保持未配置。

[下载已验收的 0.46.0 Kimi 修复版](https://github.com/aitouchidechushi/CodexBar-Dev/releases/tag/kimi-quota-fix-0.46.0-f58dfa1)：选择 Assets 中的 `CodexBar.exe`。这是用户实际验收的同一份未签名 Windows x64 Debug 程序，界面已内嵌，不依赖开发服务器；需要 WebView2 Runtime。启动前请从旧程序菜单选择“退出”，避免两个版本同时使用同一配置。

源码提交及发布标签目标：`f58dfa124d72d142491c9ff251cec90e65e009e7`。SHA256：`439B24812901822B25CDD9BB60626A78ED84660B86DF5AC2EF8163D959678D19`。

本次修复：Kimi 额度响应缺少旧式周额度时，不再导致整张卡片报错；保留实际返回的五小时额度，并按实际周期标注。用户确认此前异常的两个新订阅账号已能显示五小时额度，并确认本次不读取月额度即可发布。旧订阅继续显示其实际返回的额度，不虚构周额度或零值。本次不新增月额度读取，原有独立的网页登录月额度功能不在此次改动范围。

回归记录：前端 330 项、核心 Rust 933 项、桌面后台 545 项测试通过；类型检查、775 项语言键检查通过。此次验收不代表所有功能、升级路径和其他电脑兼容性均已验证。

## 首次上传与历史程序说明

本仓库 main 用于保存开发源码快照，包含首次上传时已有的开发修改；最新版本状态见上节。它不是“全部问题已修复”的验收声明。

[下载已确认使用的程序](https://github.com/aitouchidechushi/CodexBar-Dev/releases/tag/user-confirmed-0.45.2-20260825)：文件版本 0.45.2，所有者于 2026-09-10 确认可用。本次仅原样保存其正在使用的历史 Debug 程序，不是重新编译的正式安装包，也不代表已验证其它电脑的兼容性。需要 Windows、WebView2 和用户自己的凭据。

历史程序与当前源码不是同一次构建。发布标签和自动生成的源码压缩包指向当前开发快照，不能用于精确重建该历史程序。

## 朋友继续开发

先安装 Git、Rust stable（MSVC 工具链）、Visual Studio C++ 桌面开发工具和 Windows SDK、Node.js 20+、pnpm 10.18.1，以及 WebView2 Runtime。

```powershell
git clone https://github.com/aitouchidechushi/CodexBar-Dev.git
cd CodexBar-Dev
git switch -c codex/my-change
cd apps/desktop-tauri
pnpm install --frozen-lockfile
pnpm run tauri:dev
```

生产模式构建：在 `apps/desktop-tauri` 运行 `pnpm run tauri:build`。这是本地编译，不等于完成安装包、升级兼容性和跨电脑验证。不要同时运行历史程序和开发程序，以免单实例检查或共享配置影响调试。

检查命令（在 `apps/desktop-tauri` 中）：

```powershell
pnpm test
pnpm exec tsc --noEmit
pnpm run check-locale
cargo test --locked --manifest-path ../../rust/Cargo.toml --lib
cargo test --locked --manifest-path src-tauri/Cargo.toml --bin codexbar-desktop-tauri
```

改好后在自己的分支提交、推送并创建 Pull Request。仓库所有者需要在 GitHub Settings 的 Collaborators 中邀请朋友，朋友接受后才有直接推送权限；未获权限者可以 fork 后提交 Pull Request。公开可下载不等于任何人都可以改动本仓库。

## 上传边界与来源

首次公开快照来自 reliability-stabilization 开发工作目录，基于本地提交 `99a26b68e95fb0f2fc3b79124196768c45d57bb3` 加当前工作区改动。没有导入原仓库完整 Git 历史；原开发目录保留不变。源码、依赖锁定文件、构建脚本与许可证一起保留。

API Key、个人运行配置、浏览器数据、日志、缓存、依赖安装目录与本地内部规划记录不属于公开交付。不要把真实凭据填入测试、示例或提交记录；`.gitignore` 不会清除源码中的秘密，也不会自动取消已跟踪文件。

原项目与 Windows 移植贡献者信息见 README 和 LICENSE。上游构建/发布文档可能仍指向原仓库；不要直接运行带清理、安装或上传参数的上游发布脚本。后续发布应核对目标仓库、源码提交、文件哈希及安装测试结果。
