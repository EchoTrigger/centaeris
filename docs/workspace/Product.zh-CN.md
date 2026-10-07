# Centaeris Workspace

[English](README.md) | 简体中文

Centaeris Workspace 是运行 Centaeris 智能体的托管产品，提供工作区成员管理、持久任务、托管执行、文档处理、Django 控制平面和 Web 客户端。公开源代码采用 `AGPL-3.0-only` 许可开发。

Runtime Framework 是固定到精确公共 Git 提交的 Rust 依赖。Cargo 和 Docker 构建使用同一份锁定源码，不要求相邻的 Core 工作副本，也没有跨仓 npm/Python 源码依赖。跨平台开发、升级依赖和显式本地联调见 [Core 开发指南](docs/development/CoreDependency.md)。

超级用户通过经过验证的 ZIP 安装插件；Workspace 镜像构建上下文不包含扩展源码仓库。

## 外观

在 **设置 → 通用 → 主题** 中选择跟随系统、暗色或浅色。默认跟随系统；手动选择会在当前设备记住，并优先于系统变化。正文与过程标题为 14px，过程详情、代码和表格为 13px。每个主题中的过程标题和内容使用同一种灰色。

## 界面语言

Workspace 使用 `react-i18next` 支持英文和简体中文，默认简体中文。在**设置 → 通用 → 语言**中切换，选择会保存在当前浏览器中。登录页面也提供语言选择。切换界面语言会保留草稿，不会翻译用户内容、模型回复、命令或协议标识。

翻译资源位于 `packages/web/src/locales/`。运行 `npm run test:unit --workspace packages/web` 检查资源键、插值参数和单复数。主要浏览器回归测试明确选择英文；专门的语言测试覆盖默认中文、切换和持久化。

## 开发

```sh
cp .env.example .env
# 填写所有留空的密钥。
uv sync --locked
npm ci
cargo check --workspace --locked
docker compose config --quiet
```

启动完整本地服务栈：

```sh
docker compose build && docker compose up -d
```

命令会先构建所需的执行镜像和文档处理镜像，再启动持久服务。这些镜像是正常运行所必需的组成部分。Runtime 会先将配置的执行镜像解析为不可变 Docker 标识，再授权 AgentRun。

运行 `python scripts/ci.py`（Python 3.12；macOS/Linux 可使用 `python3`） 执行全部本地检查。文档入口为 [docs/README.md](docs/README.md)，文档正文目前主要使用英文。

捆绑 Web 字体的版权、来源和许可证记录见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)；其链接的许可证文件包含在部署的 Web 产物中。

## 参与贡献

欢迎通过 Issue 提交错误报告、自然语言复现步骤、脱敏日志、功能请求和高层设计建议。目前暂不接收用于合入项目的外部代码、补丁、文档草稿或其他作品。Pull Request 仅供协作者进行维护者开发。

临时贡献政策，以及未来贡献和商业许可计划，见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

除文件或声明另有说明外，本仓库原创源代码和文档采用 [GNU Affero General Public License v3.0 only](LICENSE) 许可。

Centaeris 名称、标志和官方视觉标识不在 AGPL 授权范围内，软件许可证不授予商标权。第三方材料保留各自声明的许可证。
