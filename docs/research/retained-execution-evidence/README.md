# Retained execution：实验与研究资料

本包记录 PR [#36](https://github.com/EchoTrigger/centaeris/pull/36) 的同实例连续执行、任务历史、Core 结果保留和重启恢复实验。研究分支从 `main` 独立建立，保持 Draft，供独立核对与后续论文分析使用。

实现候选：`25d3a95087a7662eb67610b235440a0b6c6a8264`。实机长测的产品源码为 `735b1c2ec651c095f8efb3dccfafb948f2e881c6`；之后只修改验收驱动和两份 API 测试文件。各来源、二进制摘要和文件变换分别记录在 `bundle.json`、`provenance.json`、`validation/source-fingerprints.json`，不能把不同提交下的结果混成一次实验。

## 阅读入口

| 主题 | 数据与结果 |
| --- | --- |
| 同实例 A/B 与跨 Run | `tables/lifecycle_runs.csv`：100 次完成、释放和逐次资源观测；原始 JSON 在 `data/ab-and-100-uses/` |
| 长程操作与恢复 | `data/long-run-1000-final/`：1000 个不同调用、精确有序的副作用文件、AGENTS 观测、真实 Runtime 重启和原摘要复用 |
| 内存曲线与历史对照 | `tables/memory_ablation_series.csv`：保留进程生命周期边界；原始 JSONL 与历史失败记录在 `data/` |
| Core 结果生命周期 | `tables/core_turn_scaling.csv`：100、1000、10000 次提交各保留一份完整当前快照；收据身份随轮数线性增加 |
| 门禁与 CI 修复 | `validation/`：130 个 PG 执行身份、72 个必选身份、两项 CI 测试缺陷的红绿回归，以及固定候选的远端 CI 状态 |

`long_run_1000_steps.csv` 是长测过程中保留的 256 个任务服务采样点，来源共观察到 1427 次；它不是每一步各一份快照。1000 次完成事实与独立编号的文件副作用另外核对。`indexedRunTasks` 包括 helper 任务，最大值 3004，当前资料没有把这些额外任务逐类归因。

## 实验条件与结果

实机链路是 API → Worker → Runtime → PostgreSQL → Docker；模型由确定性 mock 驱动。Runtime 限制为 512 MiB，模型上下文为 131072，执行环境为单宿主 runc、systemd、cgroup v2。两套长测使用独立消费者、数据库和 Docker 端点，共享同一 VM 和系统 containerd。

长测完成 1000 次操作，中途在完成 706 次后重启 Runtime。跨 Run 实验完成 100 次使用和释放；修正观察驱动后，沿用原容器和已完成的前三个 Run，没有重新执行它们。两套实验最终都丢弃第一次维护响应，再用同一 operationId 对账；容器清单中目标已缺席，驻留计数归零，销毁收据唯一。

以下状态保留在资料中：

- 独立严格内存检查仍为 `incomplete-or-failed`。没有正向 OOM 事件，但计划重启附近采到了退出码 137，原检查器结果没有修改。
- 六项最终组合故障实验尚未执行，详见 `tables/fault_and_fence_matrix.csv`。PG/Linux 组件测试与这些组合实验分开计数。
- 原最终 ELF 清单有 28 个未由该组证据执行的 ignored 身份；本次真实并行回归补充执行了 `native_consumption_tests::native_consumption_production_server`，合并现有明确证据后剩 27 个，见 `validation/ignored-identities-reconciled.json`。其他 CI 调用不在没有身份核对的情况下计入。
- 历史 901 次 OOM、两次不同源码的 708 次重启后 OOM，以及主动停止的旧长测分别保存。约 805 次的说法没有被提升为本包已验证的精确完成数。901 次基线目前只有 OOM/维护快照，未包含内存时间序列。

这些是证据包的当前覆盖状态；离线校验通过不表示 PR 的全部合并条件已经满足。

## 指标口径

| 指标 | 含义与适用范围 |
| --- | --- |
| 匿名内存、RSS、cgroup current/peak | 原始采样值，按进程生命周期分别分析。重启后的低值不能证明重启前释放；采样最大值也不等于连续时间绝对上界 |
| `retainedFullSnapshots` | Run 结果收集器保留的完整快照数。当前 Session 历史仍可能增长，普通快照读写的冷热分离是后续工作 |
| `controlDirectoryBytes` | 控制目录的实际占用，包含 SQLite 等文件，并非单独的 SQLite 索引大小；该 tmpfs 索引不提供宿主掉电恢复 |
| `activeTasks` / `hotRecords` | 活跃任务上限 32、热记录上限 64；历史索引与热记录分别计量 |
| mock 模型与 tar+zstd | mock 验证执行链路及协议，不是模型能力或 Terminal-Bench 成绩；归档仍是候选组件，在线文件保护与冷恢复未启用 |

## 离线验证

使用 Python 3.12 标准库，无网络、数据库或 Docker 依赖：

```text
python -B docs/research/retained-execution-evidence/verify.py
python -B docs/research/retained-execution-evidence/test_verify.py
python -B docs/research/retained-execution-evidence/export_tables.py
```

若已有最终实现的精确 checkout，可额外比较源码正文与 Git blob：

```text
python -B docs/research/retained-execution-evidence/verify.py --source-root /path/to/implementation-checkout
```

校验器检查文件清单、原件与导出关系、跨表一致性、精确身份、工具连续性和资源核销。默认离线校验依据包内记录；指定 `--source-root` 需要 Git，并会读取外部实现源码，按 Git 声明的换行等过滤规则比较规范 blob。文件哈希用于完整性和来源关联，不独立证明实验发生过。

保留的结构化观测和内存 JSONL 为原字节副本；表格由 `export_tables.py` 生成。维护账本、容器清单和副作用文件从成功命令的 stdout 提取，并保留原记录摘要。Linux 测试日志导出测试身份和结果行；Windows 聚焦日志保留诊断栈并替换本机路径。原始命令参数、配置、数据库转储和二进制不在公开包内。
