<p align="center">
  <img src="assets/centaeris-mark.svg" width="112" alt="Centaeris 标志">
</p>

# Centaeris

[English](README.md) | 简体中文

Centaeris 是使用 Rust 编写、不依赖特定宿主的智能体运行时框架。桌面端、终端和托管产品共用会话、模型请求、工具、事件、持久化和持久续执行的运行时契约。

本仓库包含公共运行时、本地宿主和用户界面，不包含第一方商业包、技能、托管控制平面代码、凭据或客户数据。

## 外观

点击左侧侧栏开关右边的太阳／月亮按钮，在浅色和暗色之间切换。选择会在当前设备记住，并优先于系统变化；首次选择前使用系统外观。正文与过程标题为 14px，过程详情、代码和表格为 13px。每个主题中的过程标题和内容使用同一种灰色。

## 功能

- 模型、工具、会话和续执行语义由同一个运行时定义。
- 本地 Electron 和终端宿主使用严格的宿主协议。
- 类型化工具契约、执行终态、安全决策和可观测的运行时事件。
- SQLite 存储适配器和 MCP 适配器遵循运行时定义的契约。
- 支持加载扩展包和技能，不捆绑具体扩展内容。

## 仓库结构

```text
packages/
  core/             运行时语义与契约
  runtime/          共用的本地运行时宿主
  runtime_sqlite/   SQLite RuntimeStore 适配器
  mcp/              MCP 适配器
  model-catalog/    Rust 模型目录
  desktop/          Electron 宿主
  tui/              终端宿主
  ui/               共用桌面界面
```

## 当前发布范围

目前仅构建和验证 Windows x64 发布产物：

- 独立 TUI 压缩包 `centaeris-windows-x64.zip`。
- 包含 `Centaeris Desktop.exe` 的未打包 Windows 桌面程序目录。

目前尚无受支持的 macOS/Linux 下载或 Windows 安装程序。源代码可能在其他平台编译，但这不代表这些平台已获得发布支持。

## 从源码构建

环境要求：Rust 1.94.1、Node.js 22.21.0、npm 10.9.4。

```powershell
cargo test --locked -p centaeris-core query_loop
npm ci
npm run gate --workspace centaeris-ui
```

完整构建和首次运行说明见 [Windows 配置指南](docs/getting-started/Windows.md)。运行时和测试无需安装扩展包或 Skill。扩展是单独版本化的资源。

## 界面语言

桌面界面目前仅提供英文。`react-i18next` 基础和英文资源分别位于 `packages/ui/src/i18n.ts` 和 `packages/ui/src/locales/`，不展示尚未完成的语言选项。用户内容、模型回复、命令和协议标识保留原文。

## 文档

请从[文档索引](docs/README.md)开始，按用户配置、运行时概念、公共契约、开发和发布验证查阅。文档正文目前主要使用英文。

## 参与贡献

欢迎通过 Issue 提交错误报告、自然语言复现步骤、脱敏日志、功能请求和高层设计建议。目前暂不接收用于合入项目的外部代码、补丁、文档草稿或其他作品。Pull Request 仅供协作者进行维护者开发。

临时贡献政策，以及未来贡献和商业许可计划，见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

Copyright (C) 2026 EchoTrigger。所有权声明见 [COPYRIGHT](COPYRIGHT)。

除文件或声明另有说明外，本仓库原创源代码和文档采用 [GNU Affero General Public License v3.0 only](LICENSE) 许可。

Centaeris 名称、标志和官方视觉标识不在 AGPL 授权范围内，软件许可证不授予商标权。第三方材料保留各自声明的许可证。

捆绑字体的许可证索引见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
