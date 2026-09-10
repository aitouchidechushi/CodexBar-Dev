# CodexBar-Dev：使用与协作说明

## 两份交付的区别

本仓库 main 是当前开发源码快照，版本配置为 0.46.0 / build 87，包含首次上传时已有的未提交开发修改。它不是“全部问题已修复”的验收声明。

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
