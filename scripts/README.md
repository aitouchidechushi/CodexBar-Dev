# 开发工具：检查、构建、打包与发布脚本

本目录存放开发辅助工具，不是下载后直接使用的桌面程序。

| 文件 | 中文说明 |
| --- | --- |
| `dev.ps1`、`dev.sh` | 开发辅助入口脚本。 |
| `local-check.ps1` | 本地检查入口。 |
| `check-version-alignment.ps1` | 检查不同配置中的版本号是否对齐。 |
| `release-doctor.ps1` | 发布相关检查工具。 |
| `verify-windows-executables.ps1` | Windows 可执行文件检查工具。 |
| `windows-release-build.ps1` | Windows 构建与发布资产制作脚本。 |
| `windows-smoke-install.ps1` | 安装、卸载等冒烟验证脚本。 |
| `macos-windows-cross-build.sh` | 从 macOS 进行 Windows 交叉构建的辅助脚本。 |

这些脚本不能不看参数就直接运行。特别是发布、安装测试和构建工作目录管理可能涉及下载、安装、卸载或清理操作；执行前确认脚本说明、目标目录、仓库地址及权限。继承的默认值不一定适用于本仓库。

初次参与开发，先看[使用与协作说明](../PUBLICATION.md)。
