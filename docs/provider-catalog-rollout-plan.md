# Provider catalog and logo rollout plan

Status: implementation in two isolated worktrees. The provider and model shortlist below was approved on 2026-09-24. Core and Workspace source changes are local; no production credential, database, pinned Core revision, or deployment has been changed.

## Second wave: approved catalog additions

The maintainer approved three more direct vendor APIs and one separately identified API-exported plan on 2026-09-24. The catalog now contains these fixed entries in addition to the first-wave entries below:

| Entry | Models | API route |
| --- | --- | --- |
| xAI | `grok-4.7` | Responses at `https://api.x.ai/v1` |
| Mistral | `mistral-small-2603`, `mistral-medium-3-5` | Chat Completions at `https://api.mistral.ai/v1` |
| Google Gemini | `gemini-3.8-flash`, `gemini-3.1-pro-preview` | OpenAI-compatible Chat Completions at `https://generativelanguage.googleapis.com/v1beta/openai` |
| Command Code GOAT | `deepseek/deepseek-v4.1-flash`, `z-ai/glm-5.3-flash`, `xiaomi/mimo-v2.6-flash`, `MiniMaxAI/MiniMax-M3` | Responses at `https://api.commandcode.ai/provider/v1` |

GOAT is a distinct Coding Plan entry with its own credential and four-model allowance. OpenCode Go remains at its approved two models. Command Code Go has no Provider API access and is not a catalog entry. The Command Code logomark comes from its official brand assets; the three direct vendor marks come from the same pi-web sprite used in the first wave.

Gemini's Chat Completions tool calls carry an opaque `extra_content.google.thought_signature`. Core preserves it in continuation state and replays it on the next request; imported history from another model uses Google's documented placeholder. Workspace's hosted Chat adapter applies the same projection. The stream parser accepts tool calls with a terminal `stop` reason as well as `tool_calls`. xAI keeps the Responses cache routing key, while OpenAI's 24-hour cache retention parameter is sent only to OpenAI.

The Core catalog and UI projections have no per-model enable switch. New templates reach Workspace through the runtime catalog API; existing Workspace instances require an administrator reconciliation preview and apply. The Core revision must be publicly reachable before Workspace pins it. Live provider calls, including multi-turn tool continuation and streaming for Gemini's beta-compatible endpoint, remain release gates before deployment.

## Implementation checkpoint

Core now has fifteen direct API entries, explicit direct/Coding Plan/Token Plan tiers, the two-model OpenCode Go entry, and selected SVG marks embedded once in the model-catalog package. The Desktop runtime response includes the icon and tier, and the Desktop picker groups entries accordingly. Retired Desktop active-model IDs are migrated on config load; an OpenCode Go selection outside the two-model allowance is cleared while its credential remains untouched. The MiniMax Anthropic adapter preserves streamed content blocks, including thinking signatures, for the next tool-call turn.

Workspace now projects the same catalog icon and tier into admin templates, renders icons in its provider list and picker, and exposes an administrator-only reconciliation preview/apply API at `/api/admin/model-catalog-reconciliation`. Apply requires the digest from a fresh preview, blocks retired models with queued/running runs and route changes, and creates model revisions while retaining credential and historical rows. The UI does not yet invoke Apply; release operators must review the preview and apply it during a controlled rollout.

The second-wave local Core Release gate passed, including the Core workspace tests, Desktop distribution and window smoke checks, and TUI package build. The Workspace adapter passed all 494 Django tests and the frontend lint, typecheck, and production build. Workspace's full `scripts/ci.ps1` still stops at its pinned-local-Core check because the neighboring original Core checkout does not match `core-revision.txt`; this does not change the successful checks run separately. Live provider calls and a production database preview remain release gates. The Workspace `core-revision.txt` still points to the prior Core commit; update it only after the Core change has a reviewable reachable commit.

A read-only snapshot of the running Workspace database on 2026-09-24 found one instantiated preset: DeepSeek with current `deepseek-v4-pro` and `deepseek-v4-flash`. The proposed catalog would retire both rows and add `deepseek-flash`; 23 historical runs reference the old Flash row (20 completed, 3 failed), and no queued/running run referenced either old row at the time of the query. This is a snapshot, not the new API's authoritative reconciliation preview. Re-run the preview after deploying the new runtime and before Apply.

## Scope agreed with the user

The first tier is twelve direct, pay-as-you-go vendor API entries. China and international accounts remain separate entries. Coding and token plans remain a separate tier. Workspace continues to use administrator-owned API keys; Workspace OAuth is out of scope. OpenCode Go exposes only `glm-5.3-flash` and `deepseek-v4.1-flash`.

| Direct vendor entry | Exact model IDs | Wire protocol |
| --- | --- | --- |
| OpenAI | `gpt-6-sol`, `gpt-6-luna`, `gpt-6-astra` | OpenAI Responses |
| Anthropic | `claude-opus-5-5`, `claude-sonnet-5`, `claude-fable-5-1` | Anthropic Messages |
| DeepSeek | `deepseek-flash` | OpenAI Chat Completions |
| Moonshot international | `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k3` | OpenAI Chat Completions |
| Moonshot China | `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k3` | OpenAI Chat Completions |
| MiniMax international | `MiniMax-M3`, `MiniMax-M2.7` | Anthropic Messages |
| MiniMax China | `MiniMax-M3`, `MiniMax-M2.7` | Anthropic Messages |
| Xiaomi MiMo | `mimo-v2.6-flash`, `mimo-v2.6-pro` | OpenAI Chat Completions |
| Z.AI international standard API | `glm-5.3-flash`, `glm-5.3` | OpenAI Chat Completions |
| BigModel China standard API | `glm-5.3-flash`, `glm-5.3` | OpenAI Chat Completions |
| Alibaba Cloud Model Studio China pay-as-you-go | `qwen3.8-flash`, `qwen3.8-max` | OpenAI Chat Completions |
| Alibaba Cloud Model Studio Singapore pay-as-you-go | `qwen3.8-flash`, `qwen3.8-max` | OpenAI Chat Completions |

The costly flagship entries `gpt-6-astra`, `claude-fable-5-1`, and `kimi-k3` were included in the approved shortlist. No separate per-model enable layer is proposed. The OpenCode Go restriction is an actual two-model catalog, not a filter over its full directory.

## Identity and endpoint rules

The Core source of truth is `packages/model-catalog/centaeris_model_catalog/catalog.json`. Keep stable IDs for existing direct vendor entries (`openai.default`, `anthropic.default`, `deepseek.default`, `moonshot.default`, `kimi.default`, `minimax.default`, `minimax-cn.default`, `xiaomi.default`). Add new IDs for direct Z.AI, direct BigModel, and two Alibaba pay-as-you-go entries. Do not repurpose `zai.default`: it currently points to the Z.AI Coding Plan endpoint, and an existing credential or active selection may reference that ID. Rename its display text to clarify the plan tier, while keeping the ID stable. Likewise, keep existing Qwen Token Plan IDs separate from new Alibaba pay-as-you-go entries. Do not silently change a saved key's billing endpoint.

The proposed new catalog IDs are `zai_standard`, `bigmodel_standard`, `alibaba_cn`, and `alibaba_intl`. Exact `providerId` names should be chosen once in the catalog and then treated as persistent identities. Z.AI standard uses `https://api.z.ai/api/paas/v4`; BigModel standard uses `https://open.bigmodel.cn/api/paas/v4`. Xiaomi's documented Chat Completions base is `https://api.xiaomimimo.com/v1`. OpenCode Go's two approved IDs both use its Chat Completions endpoint. Its `deepseek-v4.1-flash` ID must not be copied into the direct DeepSeek entry, where the documented ID is `deepseek-flash`.

Pi's current `zai` and `zai-coding-cn` entries are both Coding Plan routes (`https://api.z.ai/api/coding/paas/v4` and `https://open.bigmodel.cn/api/coding/paas/v4`). Pi has no independent standard/pay-as-you-go Z.AI or BigModel provider. Its `zai` name is therefore not a safe template for the direct API tier. The two new standard entries are regional/account routes for the GLM vendor family, while the two Coding Plan entries remain a separate billing tier. pi-web maps the two Pi Coding Plan IDs to the same `zai` icon and has an additional `zhipu` icon mapping; that icon map does not create a Pi provider.

| Entry | API base | pi-web mark |
| --- | --- | --- |
| OpenAI | `https://api.openai.com/v1` | `openai` |
| Anthropic | `https://api.anthropic.com/v1` | `anthropic` |
| DeepSeek | `https://api.deepseek.com` | `deepseek` |
| Moonshot international / China | `https://api.moonshot.ai/v1` / `https://api.moonshot.cn/v1` | `moonshot` |
| MiniMax international / China | `https://api.minimax.io/anthropic` / documented `https://api.minimax.cn/anthropic` (compatibility check pending) | `minimax` |
| Xiaomi MiMo | `https://api.xiaomimimo.com/v1` | `xiaomimimo` |
| Z.AI international / BigModel China standard | `https://api.z.ai/api/paas/v4` / `https://open.bigmodel.cn/api/paas/v4` | `zai` / `zhipu` |
| Alibaba Model Studio China / Singapore | workspace-specific regional URL below | `qwen` |

Alibaba pay-as-you-go keys and endpoints are regional. The recommended production base includes a Workspace ID: Beijing `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`; Singapore `https://{WorkspaceId}.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`. The current catalog uses Alibaba's existing regional DashScope domains as working fixed defaults, since Core has no per-account API-base setting for built-in providers. Before production use, decide whether to add the dedicated Workspace ID route; never confuse these entries with Token Plan URLs. The MiniMax China documentation now shows `https://api.minimax.cn/anthropic`, while the catalog retains `https://api.minimaxi.com/anthropic` to keep saved accounts stable. Both unauthenticated `/v1/messages` routes returned HTTP 401 on 2026-09-24, which confirms that both hosts respond but does not prove keyed compatibility; a credentialed test remains necessary before a route change.

## Logo delivery from one upstream source

Use pi-web's `public/provider-icons.svg` as the visual reference. Extract only the approved vendor marks: `openai`, `anthropic`, `deepseek`, `moonshot`, `minimax`, `xiaomimimo`, `zai`, `zhipu`, and `qwen`; the plan tier also needs `opencode` and `kimi`. Keep its MIT attribution with any copied paths. Put the selected SVG assets and provider-to-icon mapping in the Core model-catalog package. Expose the resolved icon data through the Core runtime catalog/config responses so the Desktop UI and Workspace UI render the same source. Show icons beside provider names in both the list and add-provider picker, with a text fallback for custom providers. The mark is decorative; the text remains the accessible name. Do not copy the full pi-web sprite to both frontends as independent sources.

## Existing configuration migration

The Workspace catalog endpoint supplies templates for *new* provider instances. Existing `ModelProvider` and `ModelConfig` rows are stored in the Workspace database, so a Core catalog update alone does not update them. Implement an idempotent, administrator-run reconciliation with a preview/dry-run. It should compare by `template_id`, report provider route and model additions/removals, create a new `ModelConfig` revision when an existing model changes, and make retired models non-current while retaining historical and in-flight run references. The run keeps its frozen resolved API route and model limits, while subsequent turns can select a current model. Preserve the encrypted credential, credential version, quota binding, provider enabled state, and unrelated custom providers. Report active run IDs for visibility; do not block catalog updates because a conversation uses an older revision. If a stale browser submits a retired model ID, refresh the choices and keep its draft so the user can select a current model. Report any workspace defaults or assignments that must move to a new model. The actual Go instance must end with exactly its two approved current models.

Core Desktop also persists an active `(providerId, model)` pair. Removing a model ID without migrating that pair can make runtime configuration validation fail and offer a full reset. Add a targeted migration: map obvious replacements (`gpt-5.6-sol` to `gpt-6-sol`, `gpt-5.6-luna` to `gpt-6-luna`, `claude-opus-5` to `claude-opus-5-5`, `claude-fable-5` to `claude-fable-5-1`, `deepseek-v4-flash` to `deepseek-flash`, and MiMo 2.5 to the corresponding 2.6 model). For entries with no safe equivalent, clear only the active model selection and ask the user to choose again; retain credentials and all other settings. Validate the mapped thinking mode against the new model's supported modes. In particular, do not silently map a costly OpenCode Go model to another paid model.

## Protocol gate before release

The Core Anthropic Messages adapter currently sends `thinking: adaptive` only for the native Anthropic provider and rejects assistant reasoning history without native signatures. MiniMax's official Anthropic-compatible API requires complete thinking/text/tool-use blocks to be replayed for multi-turn tool calls; `MiniMax-M2.7` cannot turn thinking off. Repair the continuation representation and replay for this provider, then test a two-turn tool call with thinking. `MiniMax-M3` vision metadata also needs correction. Confirm Chat Completions continuation with reasoning for Kimi, MiMo, DeepSeek, and GLM, and check real endpoint responses with disposable credentials before a production rollout.

## Recommended implementation order

1. Update Core catalog entries and model metadata, keeping direct API and plan identities distinct. Add the selected logo assets and catalog mapping.
2. Fix the MiniMax continuation path and verify catalog parsing, model identity uniqueness, route validation, and Desktop persisted-selection migration.
3. Expose icons in the Desktop and Workspace provider UI through the upstream catalog; add Workspace reconciliation preview and apply, preserving saved credentials and history.
4. Build and test both worktrees against the same Core revision. Run a read-only reconciliation preview against a production database copy and resolve active-model references before changing live services.
5. After reviewing the preview, pin the new Core revision in Workspace, release the Core Desktop build and deploy Workspace. Apply reconciliation during the Workspace rollout, then verify the visible provider lists, saved credentials, and two-model Go limit.

## Primary references

- OpenAI models: https://developers.openai.com/api/docs/models
- Anthropic models: https://platform.claude.com/docs/en/models/overview
- DeepSeek models and IDs: https://api-docs.deepseek.com/quick_start/pricing/
- Moonshot global and China model pages: https://platform.kimi.ai/ and https://platform.kimi.com/
- MiniMax Anthropic-compatible API: https://platform.minimax.io/docs/api-reference/text-anthropic-api and https://platform.minimax.cn/docs/api-reference/text-anthropic-api
- Xiaomi Chat Completions API: https://mimo.mi.com/docs/en-US/api/chat/openai-api
- Z.AI and BigModel quick starts: https://docs.z.ai/guides/overview/quick-start and https://docs.bigmodel.cn/cn/guide/start/quick-start
- Alibaba endpoints and recommended models: https://docs.modelstudio.console.alibabacloud.com/en/model-studio/base-url and https://docs.modelstudio.console.alibabacloud.com/en/model-studio/text-generation-model
- OpenCode Go endpoints: https://opencode.ai/docs/go/
- xAI Grok model and Responses API: https://docs.x.ai/developers/models and https://docs.x.ai/developers/rest-api-reference/inference/responses
- Mistral model cards and Chat API: https://docs.mistral.ai/models and https://docs.mistral.ai/api
- Gemini model list and OpenAI compatibility: https://ai.google.dev/gemini-api/docs/models and https://ai.google.dev/gemini-api/docs/openai
- Command Code GOAT and Provider API: https://commandcode.ai/docs/plans/goat and https://commandcode.ai/docs/provider
- Command Code brand assets: https://commandcode.ai/brand
