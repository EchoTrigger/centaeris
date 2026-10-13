# 持久执行与工作区保护：研究证据包

这是供独立研究与复核使用的数据快照，不是部署方案或性能认证。它对应 [PR #34](https://github.com/EchoTrigger/centaeris/pull/34) 的设计依据及后续测试修复；该 PR 的代码版本固定为 [`d9459a2`](https://github.com/EchoTrigger/centaeris/tree/d9459a29ee77dcca9ad9a21f93699c49e401c496)。资料分支从 `main` 的 `4c8e59f` 独立建立，不包含 PR #34 的产品改动，也不要求合并。

先读本页，再看 [研究问题与复现边界](REVIEW_GUIDE.md)、[逐操作 CSV](tables/operations.csv)。需要追溯时，使用对应 JSON、JSONL 与日志。所有数据来自合成工作负载或测试；没有客户数据、生产配置、凭据和运行时内存镜像。

## 数据结构与来源

```text
bundle.json                  证据集身份、代码版本、纳入与未纳入范围
provenance.json              原文件哈希、导出文件哈希、字段投影与路径替换记录
inventory.json              本包每个文件的 SHA-256 和字节大小
data/full-snapshot/          原完整快照路径：6 场景、12 操作、24 条计数观测
data/conditional-snapshot/   条件与强制完整快照配对：7 场景、22 操作、44 条观测
data/runsc-process/          较早的小型进程恢复实验
data/runsc-process/guarded/  加入挂载防护后的独立恢复实验
tables/operations.csv        从两组快照原始数据生成的 34 行操作表
validation/                 GitHub 失败摘录、受控复现、修复测试和 Rust 检查日志
verify.py                   离线校验器，不连接网络，不启动测试环境
```

关联层次是 **证据集 → 实验 → 工作负载 → 操作 → 指标**。`operationId` 只在所属实验中唯一；不能把两个实验的 `0-baseline` 合并。同一操作在 JSON 中有退出码、stdout 字节数、文件哈希和恢复结果，在 JSONL 中有两条计数：`sourceReadBytes`、`scannedEntries`。验证器检查关联是否完整、重复和遗漏。

例如，条件快照的 `0-conditional` 属于 `unchanged` 工作负载：请求 JSON 为 674 字节，stdout 为 290 字节，退出码 0，读取正文 536,879,122 字节。返回值是绑定已有快照的收据；没有新正文帧，恢复使用已验证的基线正文。

### 指标口径

| 字段 | 含义与单位 | 不能解释成什么 |
|---|---|---|
| `stdoutBytes` | 单次 helper 标准输出字节数；条件结果包括收据 | 网络线上实际字节、对象存储写入字节 |
| `inputRequestBytes` | 本包保留的条件请求 JSON 文件字节数 | 全链路请求、Docker 协议开销 |
| `sourceReadBytes` | 应用实际读取返回的字节数，累计散列与验证两遍读取 | 磁盘设备读取量；缓存命中仍计入 |
| `expandedBytes` | 被选中快照的文件正文大小 | 修改量或新增存储量；拒绝时留空 |
| `scannedEntries` | helper 扫描条目数 | 只包含普通文件的数量 |
| `externalCaptureElapsedMs` | 探针计时，包含 Docker exec 与探针文件 I/O | Runtime 内部延迟、p95/p99、纯散列时间 |
| `restoreStatus` | 对选定正文进行恢复并核对文件大小、哈希的结果 | 任意进程恢复或应用业务恢复 |
| `retainedLogicalBytes` | runsc 检查点文件逻辑大小总和 | 网络传输量、变化页增量或对象存储计费 |

CSV 空值表示该字段未测量或不适用，不能转换为 0。错误操作可以有非零读取量或部分输出；不能将它当成可发布快照。未变化工作负载的“输出/修改量”分母为 0，比例未定义。

## 快照实验测到了什么

条件快照实验使用真实 Linux/runsc 容器，单容器 1 GiB 内存、1 CPU、128 PID、无网络；[资源记录](data/conditional-snapshot/container-limits.json) 保留了 cgroup 实际限制。六个主场景包含 256 MiB 的 `cold.bin`、4 KiB 的 `hot.bin` 和 9 字节日志。受保护输入场景另用 8 KiB 冷文件，不能与主场景混算。

| 工作负载 | 条件 stdout 字节 | 配对完整帧字节 | 条件正文读取字节 | 验证结果 |
|---|---:|---:|---:|---|
| 内容未变化 | 290 | 268,440,053 | 536,879,122 | 恢复基线成功 |
| 替换 4 KiB 热文件 | 268,440,273 | 268,440,053 | 536,879,122 | 恢复新正文成功 |
| 大文件内覆盖 4 KiB | 268,440,273 | 268,440,053 | 536,879,122 | 恢复新正文成功 |
| 追加 3,584 字节 | 268,443,860 | 268,443,640 | 536,886,290 | 恢复新正文成功 |
| 重命名、删除、重建相同内容 | 290 | 268,440,053 | 536,879,122 | 恢复基线成功 |
| 活跃后台写入 | 0 | 未采集 | 0 | 扫描前拒绝，未尝试恢复 |

受保护输入的 materialize/revoke 返回 306/301 字节收据，配对完整帧均为 12,784 字节；恢复后排除该输入。这组共 7 个场景、6 次成功恢复。全部拥有的实验容器已清理，见 [outcome](data/conditional-snapshot/outcome.json)。

这个数据支持**在已持有匹配基线的捕获中，省掉未变化正文的传输与临时文件写入**。两遍正文读取仍然存在。请求清单、收据和协议开销仍然存在；真实 4 KiB 修改仍产生完整帧，另加 220 字节收据。本实验未测 API 上传、WAL、GC、物理磁盘 I/O 或生产费用。

Runtime 的“无正文 spool”由 [测试日志](validation/trigger-fix-runtime.txt) 中 `unchanged_receipt_does_not_create_or_write_a_snapshot_spool` 等行为测试支持，不能伪装成这次 helper 实验直接采集的 Runtime 指标。已有活动代数机制完全跳过捕获的收益，也不能算到条件快照上。

历史完整快照实验使用不同 helper、镜像和旧探针。其后台写场景在读取 268,439,561 字节、输出 492 字节部分帧后拒绝；条件实验在活动进程检查处直接拒绝。两者不是同一个注入点，不能把差异解释为性能提升。严格比较优先使用**条件实验内部的 forced-full 配对**。

## 进程恢复实验测到了什么

[guarded 实验](data/runsc-process/guarded/manifest.json) 是单主机、应用确认冻结、tmpfs 文件系统上的微型程序恢复：删除源 sandbox 后，新 sandbox 的外部 oracle 匹配，实际 pipe、普通文件、已 unlink 打开文件和链接校验通过。涵盖内存状态、进程树、线程/mmap、FD 游标及受控后台写入。

检查点逻辑大小为 389,396 字节、已分配大小为 390,144 字节；一次 restore 外部计时约 675.46 ms。较早实验的对应逻辑大小为 389,489 字节。这是两次独立 correctness smoke，不能据此给出分位数或把它与 256 MiB 文件实验排速度名次。

`capability-matrix.json` 中 `supported` 仅指该探针场景。三项损坏/身份错误均由 **probe admission** 拒绝，未证明 runsc 本身处理损坏镜像的行为。跨主机恢复、未冻结捕获、外部连接恢复均为 `not-tested`。完整产品环境生命周期、后台任务监督、宿主机崩溃恢复、对象存储和 Firecracker 尚无这份数据支持的实现结论。

## CI 与修复证据

[初始 GitHub 日志摘录](validation/github-initial-failure.txt) 对应提交 `db3428b`：1,272 项全部执行、1 项失败、无跳过。失败是客户端超时释放槽位后，测试过早断言服务端数据库任务已完成，得到 0 而不是 1。[原运行](https://github.com/EchoTrigger/centaeris/actions/runs/38351261692)

`trigger-timeout-red.txt` 是未提交的受控复现探针，不是发布提交的原样结果。两个 `trigger-fix-integration*.txt` 是中间失败版本；它们暴露了响应丢失场景还会取消服务端探针。**不要将这些中间测试合并成最终通过率。**

最终 `d9459a2` 的 [两个真实 PostgreSQL/HTTP 集成用例](validation/trigger-fix-cancellation.txt) 均通过，Core query_loop 91 项、Runtime 312 项以及 Clippy 通过；Runtime 另有 95 项环境测试忽略，不能算作执行覆盖。修改只涉及测试探针，保留生产超时与执行行为。

本地全量 API 测试按维护者要求中断，没有最终通过结论。[修复提交的 CI](https://github.com/EchoTrigger/centaeris/actions/runs/38367434641) 已发起；本包不读取或断言其最终结果。

## 离线复核

在本目录执行 `python -B verify.py`，只需 Python 标准库。验证器核对文件哈希、34 个操作与 68 条原始观测的完整关联、两遍读取字节、快照恢复文件身份、两次进程 oracle 和检查点大小。`python -B verify.py --write-table` 可从原始数据重建 CSV；它不刷新哈希清单，也不启动实验。

执行 `python -B test_verify.py` 可重复四项校验器行为测试：完整包通过、原始记录被改时拒绝、同步改写导出哈希仍不能隐藏错误字节数、缺失指标不能被当成零。反例只修改临时副本。

哈希证明“本包内容没有变化”，不能独立证明原实验真实性。`provenance.json` 区分原件与导出件；路径替换、CPU 元数据投影均有记录。原件在本地另有可恢复归档。大型正文、二进制和原始命令记录未上传，因此这是一份可核算、可审查的证据包，不是可直接恢复历史环境的镜像。
