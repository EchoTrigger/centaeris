# 独立审阅路径

建议先运行 `verify.py`，再按连续性、内存、故障和来源四个主题阅读。`bundle.json` 定义实验身份与分类，`inventory.json` 覆盖除自身外的全部文件。

## 连续性与副作用

从 `lifecycle_runs.csv` 选择 A、B、历史窗口后的 Run 和最后一次释放，比较 containerId、instanceId、environmentTaskId、文件摘要及控制代数。对照对应 `observed-*.json`，再核对独立的 resident-observations。每个 Run 应有唯一释放收据，驻留计数保持一。

长测的 `side-effects.txt` 必须精确等于 0000 至 0999，各一行。结合 `summary.json` 的不同 callId 计数、`automatic-instructions.jsonl` 和 `long-runtime-restart.json`，检查模型驱动进度、真实文件副作用、旧控制者拒绝以及原摘要身份。两套实验最后的 `container-inventories.json` 和 `maintenance-observations.json` 分别提供物理缺席与账本核销依据。

## 状态成本与内存

对比三种不同指标：Core 计量的保留正文/身份字节、Runtime 进程匿名内存，以及容器 cgroup 内存。Core 计量是序列化内容的结构观测，不是堆剖析。10000 次提交仍保留一份当前快照，但这份当前历史本身变大，轻量收据也继续增长。

最终长测在重启前的匿名采样峰值约 66.08 MiB，重启后约 62.28 MiB；两段内部都存在随工作量增长的趋势。历史 708 次运行的重启水合峰值显著更高。源码和故障条件同时变化，这些资料是历史诊断对照，不能作为只改变一个变量的受控消融实验。

严格内存检查的退出 137 记录、观测缺口和原始结果保留在 `memory-analysis.json`。本包没有通过重算状态来消除这个未完成条件。

## 故障与完成边界

先核对 72 个 PG 必选身份确实包含在 130 个已执行身份中。再看 `fault_and_fence_matrix.csv`：六项最终组合故障仍为 unrun。已有组件门禁不自动覆盖 API 到 Docker 的每个故障组合。

CI 修复分两个提交：并行测试的 transport 包装器原样传递 heartbeat timeout；Runtime schema 清理保持 serialized rollback。两条新增回归先失败，修复后九项真实 PG/Native 回归全部通过。第一次隔离镜像缺编译器，补齐后冷编译又使第一个测试 Run 过期；这些运行不计为通过。最终成功重跑没有调整队列期限、生产租约或断言。

## 可继续研究的问题

1. 当前结果生命周期、逐条恢复水合，与完整历史退出 Session 热状态之间还隔着哪些读写成本？本包能区分保留峰值与累计扫描，不能替代独立堆分析。
2. 活跃任务、热缓存、重放索引和输出如何分别消耗控制区与输出区？接近容量时，新准入拒绝和维护保留空间应分别验证。
3. 租约失效、失权和物理实例消失之间，哪些事实足以允许核销？对未知 Docker 状态保持 pending 的规则需要对应故障实验。
4. 从同一活实例连续使用，走向持久工作卷或跨宿主恢复，还需哪些生产调用方、文件一致性和资源成本证据？本 PR 的 tmpfs 与归档候选不能代替这些条件。

## 复现与下一步

获取 `bundle.json` 指定的实现提交，按照该提交下 `scripts/workspace/retained_acceptance/README.md` 创建独占测试栈。配置、端点和临时数据库应自行生成；不要套用现有 Workspace 的数据库。源码指纹覆盖驱动、模型、任务服务、Core 恢复和必选 PG 清单，可用 `--source-root` 比较。

实机长测仍需 Linux、Docker、委托 cgroup v2 和 runc。执行前核对 tmpfs 丢失范围，结束时沿原维护 operationId 确认物理缺席后核销。Core 与 PG 的窄回归可先运行，再选择是否重做长测。

真实 Workspace 的 Terminal-Bench 适配器是 #36 合并后的独立工作。这里的确定性模型结果不换算为 Terminal-Bench 成绩。
