export const developerGuides = {
  en: {
    quickstart: `
1. In **Settings → Developer → Console**, enter a credential name and choose a model. Plugins and materials are optional; leave them empty for this first call.
2. Save the secret in your backend's secret store and export it as \`CENTAERIS_RUN_KEY\`. This is the Workspace API credential, not the model provider's key. You can copy it from the credential list until you leave the console.
3. Run the complete example below in **Bash**, with curl and Python 3 installed. No repository checkout or SDK is needed. It saves the operation ID, request, endpoint, credential fingerprint, headers and response in \`./centaeris-first-call\`; the secret stays in the environment. The fingerprint lets the status example reject an accidentally changed credential before making a request.

The example refuses to reuse an existing directory, so copying it again cannot accidentally start another run. Keep the directory after a timeout and use the status example below. Choose a different directory only for an intentional new call.
`,
    firstResult: `
### Recognize the result

The first accepted call returns \`200 text/event-stream\`. Read \`runAccepted\`, join \`outputText\` chunks, and require \`runFinished\` with both \`runStatus: completed\` and \`deliveryStatus: completed\`. Text is emitted after execution ends, not as live model tokens. Failed or cancelled runs may contain only the last partial assistant response. A successful curl exit or HTTP \`200\` alone does not establish run success.

Before sending, retain the **original credential reference, endpoint, operation ID and request body**. After acceptance, also retain the Run ID and receipt times. Keep the received text and verified files in your application. The status endpoint cannot recover text or files from a disconnected stream.
`,
    recovery: `
The lookup is read-only. A \`200 application/json\` response is a status receipt, even when \`runStatus\` is \`completed\`; it contains no output. A \`404\` immediately after a timeout can race acceptance. If you retry submission, reuse the **same credential, saved operation ID and saved request body**. Never generate a new key just to recover a timed-out call.

Connection loss stops delivery; the Run continues. Repeating POST returns only a receipt if the original request was accepted. Lost output cannot be replayed. Check external side effects before deliberately generating a new run. The **Lifecycle**, **Files** and **Errors** pages explain cancellation, file verification and the other recovery outcomes.
`,
    authorization: `
### Send the credential from your backend

Set the \`Authorization: Bearer <secret>\` header on submit, status and cancel requests. Omit Workspace session cookies. Keep the secret in your backend, script or secret store.

### Choose the scope when issuing a credential

Only active Workspace owners and administrators can create, list and revoke credentials. Each credential binds to its issuer's membership, one Workspace, and one enabled model configuration. The API returns the secret at creation. The console can copy it again from the list while it stays open.

If credential creation times out, refresh the list to reconcile whether it was created. Refreshing cannot reveal the secret. Revoke an unusable credential whose secret was lost, then create a replacement.

Choose the plugins and Workspace material files the caller needs. A run can use only the selected plugins while they remain available. To attach material, include its ID in \`sourceObjectRefs\`, sorted and without duplicates. The material must remain ready and readable by the issuer. An empty scope grants no plugins or material. Personal-library access is disabled.

Revoking the credential, losing the issuing membership or admin role, or losing access to its model invalidates the credential. The service stops delivery and requests cancellation of active runs when it detects lost credential authority. To change scope, issue a new credential and revoke the old one.

Invalidation also blocks status and cancel requests. A replacement credential cannot query or cancel operations owned by the old credential, even in the same Workspace. For routine rotation, settle and save the old credential's results first. Revoke a compromised credential immediately; revocation does not undo side effects already sent to external services.
`,
    lifecycle: `
### One request starts one run

The service keeps a private temporary execution session. The initial response delivers the output and the status endpoint returns a minimal receipt. Closing the connection ends delivery. The run continues until completion, cancellation or its deadline.

The ten-minute deadline starts at acceptance and includes queue time. A client's connection or read timeout does not change it. To stop a run, send an empty \`POST /api/v1/agent-runs/{runId}/cancel\` with the original credential. A \`202\` acknowledges the cancellation request; it does not mean the Run is cancelled. A \`200\` returns the terminal receipt. Check the original operation until it reaches a terminal state. Completion can win a cancellation race, and cancellation does not roll back external side effects. A missing resource or an ordinary tool error may be returned to the model within the same run. Uncertain tool outcomes stay attached to that run's identity.

### When data is removed

The delivery window closes when the stream ends or its lease expires. After a terminal state the window is at most 60 seconds. The service reclaims the execution instance and waits for cleanup and host retirement before deleting the temporary session, inputs, events, run-owned files, snapshots and execution authorization. The status receipt reports \`cleanupStatus\`.

The receipt keeps operation and run IDs, acceptance, deadline and expiry times, run and cleanup states, and a terminal reason when available. The service keeps an input digest for conflict detection. The receipt expires 24 hours after the timestamp in the original operation ID and becomes eligible for removal once cleanup completes. Immutable physical cleanup provenance is kept separately.

These are separate limits: the client's wait, the Run's execution deadline, the original stream's delivery lease, and the operation's lookup/duplicate-prevention window. \`expiresAt\` is not a result-download deadline. Generate the key immediately before the first submit; a nearly expired unused key can be accepted with little lookup time left.

The reset policy for resumable interactive sessions is separate from one-shot run cleanup.
`,
    events: `
### Submit and read the first response

Send \`Authorization: Bearer <secret>\` on every endpoint, without Workspace session cookies. Use a new \`Idempotency-Key\` of the form \`<Unix-seconds>.<lowercase UUIDv4>\` for each intentional run. The key is valid for 24 hours from its timestamp and allows up to five minutes of future clock skew. It must stay unchanged on retries. A prompt is required. To attach material, pass sorted, unique IDs in \`sourceObjectRefs\`. The API rejects unknown fields and query parameters.

Inspect \`Content-Type\` before reading the body: the first accepted POST returns \`200 text/event-stream\`; an already accepted operation with the same credential, key and input returns \`200 application/json\` with an operation receipt. HTTP status alone cannot select a parser or establish completion. \`Accept\` does not change this behavior.

1. Read \`runAccepted\` and save its IDs. \`runProgress\` reports queueing and can report running or delivery. SSE comments keep the connection open and can be ignored.
2. Join any \`outputText\` chunks in arrival order. On success they contain the final assistant answer; a failed or cancelled run may have only a partial response.
3. If the run published files, read each artifact's start, chunks and completion or failure before reading the next artifact.
4. Read \`runFinished\` and check both \`runStatus\` and \`deliveryStatus\`.

### Check an existing operation

Query the original operation ID using the same credential to receive a JSON status receipt. Resubmitting the same key and input also returns that receipt **if the operation was already accepted**. If the original attempt never committed, the retry can be the first accepted call and return SSE instead. Always inspect \`Content-Type\`. The initial accepted POST response is the sole delivery channel for text and files. A changed input with the same key returns \`409 operation_conflict\`. An expired key returns \`410 operation_expired\`.

The request fingerprint uses the trimmed prompt and validated material IDs. Preserve the original request on retry. Deduplication prevents a second Run for that credential and operation; it does not guarantee exactly-once effects in external plugin services. A new credential is a new operation scope.
`,
    artifacts: `
### Receive and verify files

Request a published artifact in your prompt. A run can deliver up to four files, 8 MiB per file and 16 MiB of decoded bytes total. Read the files on the original SSE connection.

1. Use \`artifactStarted\` to open a temporary file for the artifact ID. Treat the filename as untrusted and confine it to the output directory.
2. Decode each \`artifactChunk\` from Base64. Check the zero-based chunk index and write the decoded bytes in order.
3. Wait for \`artifactCompleted\`. Require the artifact ID, filename, media type, byte count and SHA-256 metadata to match the start event, verify the contiguous chunk count and actual bytes, then publish the file to its destination without replacing an existing caller file.

On \`artifactFailed\`, an incomplete stream or a verification error, discard the affected partial files. Files already completed and verified can be retained. The final frame can report \`runStatus: completed\` and \`deliveryStatus: failed\` together. Status lookup gives execution and cleanup state, and the initial response is the only way to obtain the files.

### Save an editable Word file

From the Centaeris source checkout, set \`CENTAERIS_RUN_KEY\` and set \`CENTAERIS_WORKSPACE_URL\` to the Workspace base URL without \`/api/v1/agent-runs\`. This Python 3 client verifies files and writes them to \`./received\`.

\`\`\`sh
python3 scripts/workspace/one_shot_client.py \\
  --url "$CENTAERIS_WORKSPACE_URL" \\
  --prompt 'Generate an editable Word project brief and publish the final docx.' \\
  --output ./received
\`\`\`

Save the operation ID printed before submission. To inspect it without starting a run:

\`\`\`sh
python3 scripts/workspace/one_shot_client.py \\
  --url "$CENTAERIS_WORKSPACE_URL" --status --operation-id '<original-operation-id>'
\`\`\`

Submission exits \`0\` only for complete successful delivery, \`2\` for a receipt-only response, and \`1\` for a transport, protocol or delivery failure. The \`--status\` command exits \`0\` when lookup succeeds; inspect \`runStatus\` separately. For a submit retry, pass \`--operation-id\` with the original ID and keep the same prompt and credential. When the service sends \`artifactFailed\`, the client continues saving later verified files while reporting overall failure. Local verification or sequence errors stop consumption.
`,
    errors: `
### HTTP responses

| Response | What to do |
| --- | --- |
| \`400\` invalid request | Check the JSON, exact field names, prompt, material IDs and operation ID format. Fix the request after a definite validation rejection. |
| \`401\` invalid or missing credential | Check the Bearer header and use a valid Workspace API credential. |
| \`403\` credential authority lost | Ask a Workspace owner or administrator to verify the issuer's role, model access and revocation state. Treat an earlier timed-out request as uncertain. |
| \`404\` source, operation or run unavailable | Check the scope and identifiers. After a timed-out submit, status lookup may precede acceptance. If retrying, use the original credential, key and input. |
| \`409 operation_conflict\` | Check the original operation. Use a fresh key only for a deliberately different run. |
| \`410 operation_expired\` | The key can no longer submit or recover the operation. Verify external side effects before deciding whether to start a new run. |
| \`429\` from a gateway or deployment limit | Respect \`Retry-After\` when present. If acceptance is uncertain, check the original operation and reuse its key and input on retry. |
| \`5xx\`, timeout or lost submit response | If you can confirm a pre-acceptance rejection, retry the same request after the service recovers. Otherwise check the original operation first. Preserve the same credential, key and input on a submit retry. |

Example of a request rejected because its operation ID is invalid.

\`\`\`json
{"error":"operation_id_invalid"}
\`\`\`

### After acceptance

- **Run failed or was cancelled.** Read \`runStatus\` and any \`reasonCode\`. Check side effects before deliberately starting another run.
- **Stream ended before \`runFinished\`.** Check the original operation for its run status. A disconnected stream cannot be resumed.
- **Artifact delivery failed.** Keep any verified files and discard partial files. The original receipt describes the run and cleanup. Recovering files requires a new intentional run after checking for side effects.
- **Cancel returned \`503\` or timed out.** Check the original operation and retry cancel against the same run ID if it is still active.
`,
  },
  "zh-CN": {
    quickstart: `
1. 在 **设置 → 开发者 → 控制台** 中填写凭证名称并选择模型。插件和资料都是可选项，首次调用可以留空。
2. 将密钥保存到后端的密钥存储，并导出环境变量 \`CENTAERIS_RUN_KEY\`。这是 Workspace API 凭证，不是模型厂商密钥。离开控制台前，可从凭证列表再次复制。
3. 在已安装 curl 和 Python 3 的 **Bash** 中运行下面的完整示例，无需下载仓库或安装 SDK。示例将操作 ID、请求正文、地址、凭证指纹、响应头和响应保存到 \`./centaeris-first-call\`；密钥只从环境变量读取。查询示例会用指纹发现误换凭证，并在发出请求前停止。

示例拒绝使用已存在的目录，避免重复复制整段命令时误建另一个运行。超时后保留目录，使用下方的查询示例。只有明确要发起新调用时，才换一个目录名。
`,
    firstResult: `
### 确认收到结果

首次接受请求会返回 \`200 text/event-stream\`。读取 \`runAccepted\`，按顺序拼接 \`outputText\`，并确认 \`runFinished\` 同时包含 \`runStatus: completed\` 和 \`deliveryStatus: completed\`。文本在执行结束后发出，不是模型实时 token；失败或取消的运行可能只包含最后一段未完成的答复。curl 正常退出或 HTTP \`200\` 都不单独代表运行成功。

发送前保留**原凭证的引用、请求地址、操作 ID 和请求正文**。接受后再保存 Run ID 和回执中的时间信息。收到的文本和校验通过的文件由调用方保存；状态接口无法找回断线时丢失的文本和文件。
`,
    recovery: `
查询不会创建运行。\`200 application/json\` 是状态回执，即使其中的 \`runStatus\` 为 \`completed\`，也不包含输出。超时后立即查到 \`404\`，仍可能是查询早于请求接受。如需重试提交，沿用**同一个凭证、保存的操作 ID 和保存的请求正文**，不能为了恢复超时请求而生成新键。

断线会结束交付，Run 仍继续执行。原请求已被接受时，重复 POST 只返回回执，无法重放丢失的输出。决定重新运行前先核对外部副作用。取消、文件校验和其他恢复情况分别见**生命周期**、**文件产物**和**错误处理**。
`,
    authorization: `
### 从调用端发送凭证

在提交、查询状态和取消请求中设置 \`Authorization: Bearer <secret>\` 请求头。请勿携带 Workspace 登录 Cookie。把密钥保存在后端、脚本运行环境或密钥存储中。

### 签发时确定授权范围

只有当前 Workspace 的所有者和管理员可以创建、列出和撤销凭证。每个凭证绑定签发人的成员身份、一个 Workspace 和一项已启用的模型配置。API 在创建时返回密钥。控制台保持打开时，可从凭证列表再次复制。

创建凭证超时后，先刷新列表核对是否已创建。刷新无法再次显示密钥；如果某个凭证已经创建但密钥丢失，应撤销这条无法使用的凭证，再创建新凭证。

只选择调用方需要的插件和 Workspace 资料文件。运行只能使用已选且仍可用的插件。要附带资料，将文件 ID 按升序排列并去重后填入 \`sourceObjectRefs\`。资料必须处于就绪状态，且签发人仍有读取权限。空范围不授予插件或资料权限。一次性运行关闭个人资料库访问。

撤销凭证、签发人失去成员身份或管理角色，或凭证绑定的模型变得不可用时，凭证会失效。服务发现凭证权限失效后会结束交付，并请求取消仍在执行的运行。要变更授权范围，可以签发新凭证并撤销旧凭证。

失效后，状态查询和取消接口同样拒绝该凭证。即使属于同一个 Workspace，新凭证也不能查询或取消旧凭证名下的操作。常规轮换应先等旧运行结束并保存结果；凭证泄露时应立即撤销。撤销不会回滚已经发送到外部服务的副作用。
`,
    lifecycle: `
### 一次请求启动一次运行

服务为执行保留一个私有临时会话。首次响应交付结果，状态接口只返回必要回执。连接断开会结束本次交付，运行仍可继续，直至完成、取消或达到期限。

运行期限从接受请求起算十分钟，包含排队时间；调用端连接或读取超时不会改变它。要停止运行，请使用原凭证发送空请求体的 \`POST /api/v1/agent-runs/{runId}/cancel\`。\`202\` 表示收到取消请求，不代表已经取消；\`200\` 返回已达到终态的回执。查询原操作直到终态。运行完成可能先于取消生效，取消也不会回滚外部副作用。普通工具错误或资源缺失可以在本轮返回给模型处理。工具执行结果未知时，保留原运行身份。

### 数据何时回收

流结束或交付租期到期时结束交付。运行达到终态后的交付窗口最多为 60 秒。服务回收执行实例，在确认环境清理和宿主退役后，删除临时会话、输入、事件、运行文件、快照和执行授权。可通过回执的 \`cleanupStatus\` 查询进度。

回执记录操作和运行 ID、接受时间、执行期限、到期时间、运行与清理状态，以及已知的终态原因。服务同时保留输入摘要用于判断冲突。回执按原操作 ID 的时间戳起算，24 小时后到期，数据清理完成后即可删除。用于证明物理清理的记录单独保留。

调用端等待时限、Run 执行期限、原流的交付租期、操作查询与去重窗口是不同边界。\`expiresAt\` 不是结果下载期限。应在首次发送前生成键；快到期但从未使用的键仍可能被接受，此时剩余查询时间很短。

可恢复交互会话的关闭后重置策略单独定义。
`,
    events: `
### 提交并读取首次响应

所有接口都使用 \`Authorization: Bearer <secret>\`，不得携带 Workspace 登录 Cookie。每次明确要启动新运行时，生成新的 \`Idempotency-Key\`，格式为 \`<Unix 秒时间戳>.<小写 UUIDv4>\`。从键内时间戳起算 24 小时有效，最多接受向未来偏移五分钟的时钟误差，重试时不得改变。请求必须提供提示词。需要附带资料时，在 \`sourceObjectRefs\` 中提供升序且去重的 ID。API 拒绝未知字段和查询参数。

读取响应正文前先检查 \`Content-Type\`：首次接受的 POST 返回 \`200 text/event-stream\`；原操作已经被接受后，相同凭证、键和输入的重试返回 \`200 application/json\` 操作回执。不能仅凭 HTTP 状态选择解析器或判断完成。\`Accept\` 不改变这个分流。

1. 读取 \`runAccepted\` 并保存其中的 ID。\`runProgress\` 报告排队，也可能报告运行或交付进度。可忽略用于保活的 SSE 注释。
2. 按接收顺序合并 \`outputText\`。成功时得到助手最终答复，失败或取消时可能只有未完成的答复。
3. 如运行发布了文件，按顺序读取每个产物的开始、分片和完成或失败事件。
4. 读取 \`runFinished\`，分别检查 \`runStatus\` 和 \`deliveryStatus\`。

### 核对已有操作

使用原凭证和原操作 ID 查询，可获得 JSON 状态回执。使用同一个键和相同输入重发，**只有原操作已经被接受时**才返回该回执；如果原尝试尚未提交成功，重试可能成为首次接受的请求并返回 SSE。因此每次都应检查 \`Content-Type\`。文本和文件只通过首次被接受的 POST 响应交付。相同键搭配不同输入会返回 \`409 operation_conflict\`。到期的键会返回 \`410 operation_expired\`。

请求指纹使用去除首尾空白后的提示词和经过校验的资料 ID。重试应保留原请求。去重保证同一凭证和操作不重复创建 Run，不保证外部插件服务的副作用恰好执行一次。更换凭证会进入另一个操作范围。
`,
    artifacts: `
### 接收并校验文件

可以在提示词中要求发布产物。单次运行最多交付四个文件，单文件上限 8 MiB，解码后总量上限 16 MiB。请从原 SSE 连接接收文件。

1. 收到 \`artifactStarted\` 后，按产物 ID 打开临时文件。将文件名视为不可信输入，限制写入目标在输出目录内。
2. 对每个 \`artifactChunk\` 进行 Base64 解码，检查从零开始的分片索引，并按顺序写入解码后的字节。
3. 收到 \`artifactCompleted\` 后，确认产物 ID、文件名、媒体类型、字节数和 SHA-256 元数据与开始事件一致，再核对连续分片数和实际字节，最后发布到目标路径，不覆盖调用方已有文件。

遇到 \`artifactFailed\`、流中断或校验失败时，丢弃受影响的未完成文件。可以保留已交付且校验通过的文件。终态事件可能同时报告 \`runStatus: completed\` 和 \`deliveryStatus: failed\`。状态查询只提供运行与清理状态，文件只能从首次响应取得。

### 保存可编辑的 Word 文件

在 Centaeris 源码目录运行下面的 Python 3 示例。设置 \`CENTAERIS_RUN_KEY\`，并将 \`CENTAERIS_WORKSPACE_URL\` 设为 Workspace 基础地址，不包含 \`/api/v1/agent-runs\`。脚本会校验文件并写入 \`./received\`。

\`\`\`sh
python3 scripts/workspace/one_shot_client.py \\
  --url "$CENTAERIS_WORKSPACE_URL" \\
  --prompt '生成一份可编辑的 Word 项目简报并发布最终的 docx 文件。' \\
  --output ./received
\`\`\`

保存提交前打印的操作 ID。以下命令只查询，不创建运行：

\`\`\`sh
python3 scripts/workspace/one_shot_client.py \\
  --url "$CENTAERIS_WORKSPACE_URL" --status --operation-id '<原操作ID>'
\`\`\`

提交命令只在完整成功交付后退出 \`0\`，只收到回执时退出 \`2\`，传输、协议或交付失败时退出 \`1\`。\`--status\` 查询成功退出 \`0\`，仍需单独检查 \`runStatus\`。重试提交时，通过 \`--operation-id\` 传入原 ID，并保留同样的提示词和凭证。服务发送 \`artifactFailed\` 时，脚本继续保存后续校验通过的文件，但本次整体交付仍报告失败；本地校验或序号错误则停止读取。
`,
    errors: `
### HTTP 响应

| 响应 | 调用方操作 |
| --- | --- |
| \`400\` 请求无效 | 检查 JSON、字段名、提示词、资料 ID 和操作 ID 格式。确认因校验被拒后，修正请求。 |
| \`401\` 凭证缺失或无效 | 检查 Bearer 请求头并使用有效的 Workspace API 凭证。 |
| \`403\` 凭证授权已失效 | 请 Workspace 所有者或管理员核对签发人角色、模型权限和凭证撤销状态。此前超时请求的接受结果仍需核实。 |
| \`404\` 资料、操作或运行不可用 | 检查授权范围和 ID。提交超时后的查询可能早于服务接受请求。如需重试，沿用原凭证、原键和原输入。 |
| \`409 operation_conflict\` | 核对原操作。只有确定要启动另一个运行时才生成新键。 |
| \`410 operation_expired\` | 该键已无法用于提交或核对。决定是否启动新运行前，先确认外部副作用。 |
| 网关或部署限流返回 \`429\` | 遵守返回的 \`Retry-After\`。接受结果未知时先核对原操作，重试沿用原键和原输入。 |
| \`5xx\`、超时或提交响应丢失 | 确认服务在接受前拒绝时，可待服务恢复后重试同一请求。接受结果不明时先查原操作。重发提交时沿用原凭证、原键和原输入。 |

操作 ID 无效时的错误示例。

\`\`\`json
{"error":"operation_id_invalid"}
\`\`\`

### 请求接受后

- **运行失败或取消。** 查看 \`runStatus\` 和已知的 \`reasonCode\`。决定启动新运行前先核实副作用。
- **未收到 \`runFinished\` 就断线。** 查询原操作的运行状态。已断开的流无法续接。
- **产物交付失败。** 保留校验通过的文件，丢弃未完成的文件。原回执只描述运行和清理。确认副作用后，才可发起新运行以重新生成文件。
- **取消请求收到 \`503\` 或超时。** 核对原操作。如果运行仍活跃，使用同一个运行 ID 重试取消。
`,
  },
};
