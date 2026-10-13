# 给独立研究者的阅读路径

目标是判断现有数据足以支持哪些改动、还有哪些实验才能决定后续架构。请分别输出：已验证事实、从事实作出的推断、缺失证据、建议实验；引用文件路径及字段，不把 `not-tested` 当成失败或通过。

## 优先研究的问题

1. **自动保存与工作区保护分开评价。** 资料库笔记已由 PR #33 引入数据库版本、幂等与本地草稿；这份快照实验不是笔记 WAL 或对象存储费用测量。笔记还需要真实正文大小分布、编辑频率、合并比例、历史保留与 GC 数据。当前包没有这些观测，不能计算它的写入放大改善比例。
2. **未变化捕获的实际发生率是多少？** 现有计数证明 helper 可以节省正文输出，却没有生产触发频率、活动代数完全跳过比例、缓存冷启动比例。应先测这些分母，再估计收益。
3. **真正修改后的全量成本如何降低？** 4 KiB 修改仍产生约 256 MiB 完整帧。比较固定块、内容定义分块、文件级复用、压缩时，要保持独立恢复、完整清单、权限与一致性约束相同；当前没有这些候选实现的数据。
4. **会话稳定身份与物理进程连续性如何衔接？** PR #34 仍是 `executionMode: perRun`，稳定逻辑 Environment 并不等于复用运行实例。需要实例租约、fencing、控制器重连、任务身份与输出恢复的真实行为测试。
5. **后台写入如何形成一致恢复点？** 文件快照拒绝活动进程；runsc 探针使用应用确认冻结。不能删除拒绝逻辑后宣称获得通用一致性。请提出进程、文件、外部副作用之间的协调协议与故障注入位置。
6. **选择 runsc 还是 Firecracker 的证据缺口？** 本包仅有 runsc 微型 correctness smoke，没有同负载 Firecracker 对照，也没有跨主机、外部 socket、磁盘卷或版本升级兼容性结论。
7. **保存多久、怎样回收？** 当前版本、活动执行、检查点与被授权输入的保护引用应分别定义。需要测保留策略下的长期增长、GC 延迟、引用提交失败及对象存储一致性；当前单次文件字节数不能回答这些问题。
8. **测试取消与业务取消怎样隔离？** `validation/` 暴露的竞态说明客户端请求结束不代表服务端工作结束。评估超时、不确定提交、幂等重试时，应以可验证事实和完成信号建模，而不是用加大 sleep/超时掩盖。

## 可用的成本模型

对实际触发的一次捕获，至少分开记录源读取、stdout/协议传输、Runtime spool、API 接收/验证/写入、数据库提交、GC。对未测量项保持未知。

可提出待验证模型：`总成本 = 捕获次数 × 各路径占比 × 各路径成本`。路径至少包括活动代数完全跳过、冷缓存完整捕获、缓存命中且不变、缓存命中但变化、失败与重试。当前只有几个路径的单次 helper 字节观测，尚不能填满该模型，更不能换算货币成本。

下一轮实验应采用固定版本与资源限制、可控冷热缓存、交错/随机化配对顺序、多个正文大小和文件数量、足够重复样本，并分离失败路径。只有在采样设计完成后才报告分位数、置信区间或吞吐量。

## 代码与构建身份

所有代码链接固定到 [PR #34 的 `d9459a2`](https://github.com/EchoTrigger/centaeris/tree/d9459a29ee77dcca9ad9a21f93699c49e401c496)，不是浮动 main。

- [条件捕获探针](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/perf/harness/conditional_snapshot_probe.py)：其 SHA-256 与条件实验 manifest 一致。
- [完整快照探针](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/perf/harness/workspace_snapshot_probe.py)：与条件实验使用的基础探针一致；历史完整实验的旧探针不同。
- [合成文件工作负载](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/perf/harness/workspace_protection.py)：哈希绑定内容构造规则。
- [runsc 探针](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/perf/harness/durable_execution_probe.py) 与 [C 程序](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/perf/workloads/durable_execution/probe_main.c)：分别匹配 guarded 实验的 harness/source 哈希。较早恢复实验的 harness 哈希不同，不声称由最终探针原样产生。
- [测试修复](https://github.com/EchoTrigger/centaeris/commit/d9459a29ee77dcca9ad9a21f93699c49e401c496)：独立提交，仅修改测试。
- [逻辑环境实现说明](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/docs/development/SessionEnvironments.md) 与 [条件捕获协议](https://github.com/EchoTrigger/centaeris/blob/d9459a29ee77dcca9ad9a21f93699c49e401c496/docs/workspace/architecture/ConditionalWorkspaceCapture.md)。

条件 helper 来自 `10fd8d9`。导出时已比较该版本与 `d9459a2` 的 helper 主文件、条件捕获模块及 Runtime 对应两个文件，内容无差异；这不是声称两棵完整源码树相同。精确二进制、镜像与 harness 哈希见 manifest 和 `provenance/conditional-build.json`。

历史 manifest 的 `sourceRevision` 是当时记录的 checkout 基线标签，部分实验包含未提交探针；不能把该字段单独当作全部输入可重建的保证。以 source/harness/binary 哈希共同辨识。旧完整快照探针的确切文件保存在本地原始证据中，本包没有复制为新的生产入口。

## 复现分为两种

**核算既有数据**：克隆本资料分支，运行 `python -B docs/research/durable-execution-evidence/verify.py`。不需要 Docker、VMware、PostgreSQL 或外部服务。

**重新进行系统实验**：在隔离 Linux 主机上，从固定源码构建 helper，准备固定镜像和有容量上限的独立实验目录；按固定版本探针的 `--help` 和安全前置条件运行。新的执行应使用新的 experimentId，保存新原始证据，不能覆盖本包或冒用旧哈希。已记录的 Docker image ID 是当时本地镜像身份，不保证可从公共 registry 拉取；本包也未提供历史二进制和检查点镜像。

如只重跑 CI 失败行为，可在专用测试 PostgreSQL 上使用 `api.test_settings` 运行 `AgentWorkRuntimeTests` 中的 `test_materializer_timeout_does_not_block_core_final` 和 `test_lost_response_and_replayed_commit_reuse_one_real_admission`。它们执行真实 Rust/HTTP/事务集成，不能只跑普通 Rust 单元测试来代替。不要向生产数据库运行测试命令。

## 纳入与排除

保留成功、预期拒绝以及中间失败证据，逐组记账。未收录全部历史烟雾测试；尤其没有将被中断的完整 API 测试、被中断的条件快照实验计为通过。这里不是独立随机样本全集，也没有生产样本。

发布前离线校验还会做两类反例：改动原始计数应触发哈希错误；即使同步改写导出哈希，破坏两遍读取/正文大小关系仍应被语义校验拒绝。这样能检查证据链内部一致性，但无法替代独立复现实验。
