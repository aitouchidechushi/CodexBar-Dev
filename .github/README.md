# GitHub 仓库协作与自动化配置

本目录管理 GitHub 上的协作规则和自动化，不属于程序运行时的业务代码。

| 路径 | 中文说明 |
| --- | --- |
| `workflows/` | GitHub Actions 工作流，定义仓库自动执行的任务。 |
| `scripts/` | 工作流使用的辅助脚本。 |
| `ISSUE_TEMPLATE/` | 提交问题反馈时使用的模板。 |
| `PULL_REQUEST_TEMPLATE.md` | 提交合并请求时使用的说明模板。 |
| `dependabot.yml` | 依赖更新检查配置。 |

修改自动化时先确认触发条件、权限和运行环境。现有 interaction guard 已改为手动触发，避免自动关闭朋友的贡献请求。

[返回仓库首页](../README.md)
