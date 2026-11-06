export const developerGuides = {
  en: {
    quickstart: `
1. In **Settings → Developer → Console**, create an API credential with a model you can use. Leave plugin and material scopes empty for the minimal request.
2. Store the secret in your caller's secret store and make it available as \`CENTAERIS_RUN_KEY\`. The console shows the secret once.
3. Run the curl example in a Unix shell with curl and Python 3. Save the operation ID it prints. The response streams a receipt, progress and the final text.
`,
    wordExample: `

### Save an editable Word file

From the Centaeris source checkout, run the Python 3 example below with \`CENTAERIS_RUN_KEY\` set. Set \`CENTAERIS_WORKSPACE_URL\` to the Workspace base URL shown in the console, without \`/api/v1/agent-runs\`. The script reads the same stream, verifies each file and writes it to \`./received\`.

\`\`\`sh
python3 scripts/workspace/one_shot_client.py \\
  --url "$CENTAERIS_WORKSPACE_URL" \\
  --prompt 'Generate an editable Word project brief and publish the final docx.' \\
  --output ./received
\`\`\`
`,
    authorization: `
### Send the credential from your backend

Set the \`Authorization: Bearer <secret>\` header on submit, status and cancel requests. Omit Workspace session cookies. Keep the secret in your backend, script or secret store.

### Choose the scope when issuing a credential

Only active Workspace owners and administrators can create, list and revoke credentials. Each credential binds to its issuer's membership, one Workspace, and one enabled model configuration. The console shows the secret only on creation.

Choose the plugins and Workspace material files the caller needs. A run can use only the selected plugins while they remain available. To attach material, include its ID in \`sourceObjectRefs\`, sorted and without duplicates. The material must remain ready and readable by the issuer. An empty scope grants no plugins or material. Personal-library access is disabled.

Revoking the credential, losing the issuing membership or admin role, or losing access to its model invalidates the credential. The service stops delivery and requests cancellation of active runs when it detects lost credential authority. To change scope, issue a new credential and revoke the old one.
`,
    lifecycle: `
### One request starts one run

The service keeps a private temporary execution session. The initial response delivers the output and the status endpoint returns a minimal receipt. Closing the connection ends delivery. The run continues until completion, cancellation or its deadline.

The ten-minute deadline starts at acceptance and includes queue time. To stop a run, send an empty \`POST /api/v1/agent-runs/{runId}/cancel\` with the original credential. A \`202\` acknowledges the cancellation request. A \`200\` returns the terminal receipt. Check the original operation until it reaches a terminal state. A missing resource or an ordinary tool error may be returned to the model within the same run. Uncertain tool outcomes stay attached to that run's identity.

### When data is removed

The delivery window closes when the stream ends or its lease expires. After a terminal state the window is at most 60 seconds. The service reclaims the execution instance and waits for cleanup and host retirement before deleting the temporary session, inputs, events, run-owned files, snapshots and execution authorization. The status receipt reports \`cleanupStatus\`.

The receipt keeps operation and run IDs, acceptance, deadline and expiry times, run and cleanup states, and a terminal reason when available. The service keeps an input digest for conflict detection. The receipt expires 24 hours after the timestamp in the original operation ID and becomes eligible for removal once cleanup completes. Immutable physical cleanup provenance is kept separately.

The reset policy for resumable interactive sessions is separate from one-shot run cleanup.
`,
    events: `
### Submit and read the first response

Use a new \`Idempotency-Key\` of the form \`<Unix-seconds>.<lowercase UUIDv4>\` for each intentional run. The key is valid for 24 hours from its timestamp and allows up to five minutes of future clock skew. A prompt is required. To attach material, pass sorted, unique IDs in \`sourceObjectRefs\`. The API rejects unknown fields and query parameters.

1. Read \`runAccepted\` and save its IDs. \`runProgress\` reports queueing and can report running or delivery. SSE comments keep the connection open and can be ignored.
2. Join any \`outputText\` chunks in arrival order. They contain the final assistant text.
3. If the run published files, read each artifact's start, chunks and completion or failure before reading the next artifact.
4. Read \`runFinished\` and check both \`runStatus\` and \`deliveryStatus\`.

### Check an existing operation

Resubmit the same key and same input or query the original operation ID using the same credential to receive a JSON status receipt. The initial POST response is the sole delivery channel for text and files. A changed input with the same key returns \`409 operation_conflict\`. An expired key returns \`410 operation_expired\`.
`,
    artifacts: `
### Receive and verify files

Request a published artifact in your prompt. A run can deliver up to four files, 8 MiB per file and 16 MiB of decoded bytes total. Read the files on the original SSE connection.

1. Use \`artifactStarted\` to open a temporary file for the artifact ID. Treat the filename as untrusted and confine it to the output directory.
2. Decode each \`artifactChunk\` from Base64. Check the zero-based chunk index and write the decoded bytes in order.
3. Wait for \`artifactCompleted\`, verify the chunk count, byte count and SHA-256 digest, then publish the file to its destination.

On \`artifactFailed\`, an incomplete stream or a verification error, discard the affected partial files. Files already completed and verified can be retained. The final frame can report \`runStatus: completed\` and \`deliveryStatus: failed\` together. Status lookup gives execution and cleanup state, and the initial response is the only way to obtain the files.
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
1. 在 **设置 → 开发者 → 控制台** 中创建 API 凭证并选择一个可用模型。最小请求可以把插件和资料范围留空。
2. 把密钥保存到调用端的密钥存储，并通过环境变量 \`CENTAERIS_RUN_KEY\` 提供。控制台只显示一次密钥。
3. 在装有 curl 和 Python 3 的 Unix shell 中运行 curl 示例，保存示例打印的操作 ID。响应会依次发送接收回执、进度和最终文本。
`,
    wordExample: `

### 保存可编辑的 Word 文件

在 Centaeris 源码目录运行下面的 Python 3 示例。设置 \`CENTAERIS_RUN_KEY\`，并将 \`CENTAERIS_WORKSPACE_URL\` 设为控制台中的 Workspace 基础地址，去掉 \`/api/v1/agent-runs\`。脚本读取同一次响应，校验文件并写入 \`./received\`。

\`\`\`sh
python3 scripts/workspace/one_shot_client.py \\
  --url "$CENTAERIS_WORKSPACE_URL" \\
  --prompt '生成一份可编辑的 Word 项目简报并发布最终的 docx 文件。' \\
  --output ./received
\`\`\`
`,
    authorization: `
### 从调用端发送凭证

在提交、查询状态和取消请求中设置 \`Authorization: Bearer <secret>\` 请求头。请勿携带 Workspace 登录 Cookie。把密钥保存在后端、脚本运行环境或密钥存储中。

### 签发时确定授权范围

只有当前 Workspace 的所有者和管理员可以创建、列出和撤销凭证。每个凭证绑定签发人的成员身份、一个 Workspace 和一项已启用的模型配置。控制台只在创建时显示密钥。

只选择调用方需要的插件和 Workspace 资料文件。运行只能使用已选且仍可用的插件。要附带资料，将文件 ID 按升序排列并去重后填入 \`sourceObjectRefs\`。资料必须处于就绪状态，且签发人仍有读取权限。空范围不授予插件或资料权限。一次性运行关闭个人资料库访问。

撤销凭证、签发人失去成员身份或管理角色，或凭证绑定的模型变得不可用时，凭证会失效。服务发现凭证权限失效后会结束交付，并请求取消仍在执行的运行。要变更授权范围，可以签发新凭证并撤销旧凭证。
`,
    lifecycle: `
### 一次请求启动一次运行

服务为执行保留一个私有临时会话。首次响应交付结果，状态接口只返回必要回执。连接断开会结束本次交付，运行仍可继续，直至完成、取消或达到期限。

运行期限从接受请求起算十分钟，包含排队时间。要停止运行，请使用原凭证发送空请求体的 \`POST /api/v1/agent-runs/{runId}/cancel\`。\`202\` 表示已收到取消请求，\`200\` 返回已达到终态的回执。可持续查询原操作直至终态。普通工具错误或资源缺失可以在本轮返回给模型处理。工具执行结果未知时，保留原运行身份。

### 数据何时回收

流结束或交付租期到期时结束交付。运行达到终态后的交付窗口最多为 60 秒。服务回收执行实例，在确认环境清理和宿主退役后，删除临时会话、输入、事件、运行文件、快照和执行授权。可通过回执的 \`cleanupStatus\` 查询进度。

回执记录操作和运行 ID、接受时间、执行期限、到期时间、运行与清理状态，以及已知的终态原因。服务同时保留输入摘要用于判断冲突。回执按原操作 ID 的时间戳起算，24 小时后到期，数据清理完成后即可删除。用于证明物理清理的记录单独保留。

可恢复交互会话的关闭后重置策略单独定义。
`,
    events: `
### 提交并读取首次响应

每次明确要启动新运行时，生成新的 \`Idempotency-Key\`，格式为 \`<Unix 秒时间戳>.<小写 UUIDv4>\`。从键内时间戳起算 24 小时有效，最多接受向未来偏移五分钟的时钟误差。请求必须提供提示词。需要附带资料时，在 \`sourceObjectRefs\` 中提供升序且去重的 ID。API 拒绝未知字段和查询参数。

1. 读取 \`runAccepted\` 并保存其中的 ID。\`runProgress\` 报告排队，也可能报告运行或交付进度。可忽略用于保活的 SSE 注释。
2. 按接收顺序合并 \`outputText\`，得到助手最终文本。
3. 如运行发布了文件，按顺序读取每个产物的开始、分片和完成或失败事件。
4. 读取 \`runFinished\`，分别检查 \`runStatus\` 和 \`deliveryStatus\`。

### 核对已有操作

使用原凭证和原操作 ID 查询，或使用同一个键和相同输入重发，都可获得 JSON 状态回执。文本和文件只通过首次 POST 响应交付。相同键搭配不同输入会返回 \`409 operation_conflict\`。到期的键会返回 \`410 operation_expired\`。
`,
    artifacts: `
### 接收并校验文件

可以在提示词中要求发布产物。单次运行最多交付四个文件，单文件上限 8 MiB，解码后总量上限 16 MiB。请从原 SSE 连接接收文件。

1. 收到 \`artifactStarted\` 后，按产物 ID 打开临时文件。将文件名视为不可信输入，限制写入目标在输出目录内。
2. 对每个 \`artifactChunk\` 进行 Base64 解码，检查从零开始的分片索引，并按顺序写入解码后的字节。
3. 收到 \`artifactCompleted\` 后，校验分片数、字节数和 SHA-256 摘要，再将文件写入最终路径。

遇到 \`artifactFailed\`、流中断或校验失败时，丢弃受影响的未完成文件。可以保留已交付且校验通过的文件。终态事件可能同时报告 \`runStatus: completed\` 和 \`deliveryStatus: failed\`。状态查询只提供运行与清理状态，文件只能从首次响应取得。
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
