<p align="center">
  <img src="assets/centaeris-mark.svg" width="112" alt="Centaeris 标志">
</p>

# Centaeris

[English](README.md) | 简体中文

Centaeris 是使用 Rust 编写、不依赖特定宿主的智能体运行时框架。桌面端、终端和托管产品共用会话、模型请求、工具、事件、持久化和持久续执行的运行时契约。

本仓库统一公共运行时、本地产品和托管产品源码，保留各组件职责及两套内置 Skill。私有插件、凭据、客户数据和本地部署状态不进入仓库。

## 外观

在 Settings > Appearance 中选择跟随系统、浅色或暗色，偏好会在当前设备记住。正文与过程标题为 14px，过程详情、代码和表格为 13px。每个主题中的过程标题和内容使用同一种灰色。

## 功能

- 模型、工具、会话和续执行语义由同一个运行时定义。
- 本地 Electron 和终端宿主使用严格的宿主协议。
- 类型化工具契约、执行终态、安全决策和可观测的运行时事件。
- SQLite 存储适配器和 MCP 适配器遵循运行时定义的契约。
- 支持加载扩展包和技能，内置三项公开的 System Skills，不捆绑商业扩展包。

## 仓库结构

| 包 | 职责 |
| --- | --- |
| [`packages/core`](packages/core/README.md) | 运行时语义与契约 |
| [`packages/agent`](packages/agent/README.md) | Rust Agent SDK 与本地执行适配器 |
| [`packages/sdk`](packages/sdk/README.md) | 类型化 TypeScript 会话客户端 |
| [`packages/desktop/src/services`](packages/desktop/src/services/README.md) | Desktop 与 CLI 共用的产品服务 |
| [`packages/sqlite`](packages/sqlite/README.md) | SQLite RuntimeStore 适配器 |
| [`packages/mcp`](packages/mcp/README.md) | MCP 适配器 |
| [`packages/model-catalog`](packages/model-catalog/README.md) | Rust 模型目录 |
| [`packages/model-http`](packages/model-http/Cargo.toml) | 共享模型 HTTP 与流传输 |
| [`packages/desktop`](packages/desktop/README.md) | Electron 宿主 |
| [`packages/cli`](packages/cli/README.md) | 交互终端、Print、JSON 和 RPC 入口 |
| [`packages/ui`](packages/ui/README.md) | 共享前端基础 |
| [`packages/visuals`](packages/visuals/package.json) | 共享可视化渲染 |
| [`packages/workspace/api`](packages/workspace/api/README.md) | 托管控制平面 |
| [`packages/workspace/runtime`](packages/workspace/runtime/README.md) | 托管 Runtime 适配器 |
| [`packages/workspace/executor`](packages/workspace/executor/README.md) | 托管执行代理与沙箱工具 |
| [`packages/workspace/web`](packages/workspace/web/README.md) | 托管 Web 界面 |
| [`packages/workspace/documents`](packages/workspace/documents/README.md) | 材料处理服务 |

`system-skills/` 为本地产品内置 Skill，`skills/system/` 为托管产品内置 Skill，两者均不属于 workspace 包。各包 README 目前仅提供英文版本。

目录及产品边界见[源码统一说明](docs/development/SourceUnification.md)，托管产品文档见[Workspace 文档索引](docs/workspace/README.md)。根目录统一依赖与 Compose 入口，各产品分别构建、验证和发行。

## 当前发布范围

本地产品目前仅构建和验证 Windows x64 发布产物：

- 面向当前用户的 Desktop 安装程序 `Centaeris-Desktop-0.1.0-windows-x64-setup.exe`。
- Desktop 便携压缩包 `centaeris-desktop-windows-x64.zip`。
- 独立 TUI 压缩包 `centaeris-windows-x64.zip`。
- 对应版本源码 `centaeris-source.zip`。

首发计划为 **v0.1.0 Windows Preview**，通过 GitHub 预发布提供。本地候选包构建完成不代表已经公开发布。安装和模型配置见 [Windows 配置指南](docs/getting-started/Windows.md)。目前尚无受支持的 macOS/Linux 下载；源代码可能在其他平台编译，但这不代表这些平台已获得发布支持。

## 从源码构建

环境要求：`rust-toolchain.toml` 指定的 Rust、Node.js 22.21.0、pnpm 12.10.1 和 Python 3.12。

```sh
python3 scripts/ci.py Source --frontend-tests
```

Windows 使用 `python`。跨平台源码检查见[开发指南](docs/development/Building.md)；完整 Windows 构建和首次运行说明见 [Windows 配置指南](docs/getting-started/Windows.md)。运行时和测试无需安装 Plugin。Desktop 与 TUI 发行包内置本仓库的公开 System Skills；其他扩展仍是单独版本化的资源。

## 界面语言

桌面界面目前仅提供英文。`react-i18next` 基础和英文资源分别位于 `packages/desktop/src/renderer/i18n.ts` 和 `packages/desktop/src/renderer/locales/`，不展示尚未完成的语言选项。用户内容、模型回复、命令和协议标识保留原文。

## 文档

请从[文档索引](docs/README.md)开始，按用户配置、运行时概念、公共契约、开发和发布验证查阅。文档正文目前主要使用英文。

## 参与贡献

欢迎贡献。小范围文档修正可直接提交 PR；Bug 修复请关联有复现步骤的 Issue。新功能、较大 UI 调整和架构变更应先通过 Issue 与维护者确认方向，再开始实现。贡献提交使用 DCO sign-off，PR 通过 CI 和审查后合入。

贡献政策及许可证说明见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

Copyright (C) 2026 EchoTrigger。所有权声明见 [COPYRIGHT](COPYRIGHT)。

除文件或声明另有说明外，本仓库原创源代码和文档采用 [GNU Affero General Public License v3.0 only](LICENSE) 许可。

所有自有包采用同一许可证。包的许可范围及第三方材料说明见 [LICENSING.md](LICENSING.md)。

Centaeris 名称、标志和官方视觉标识不在 AGPL 授权范围内，软件许可证不授予商标权。第三方材料保留各自声明的许可证。

捆绑字体的许可证索引见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
