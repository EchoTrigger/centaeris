# Provider catalog

`packages/model-catalog/centaeris_model_catalog/catalog.json` owns built-in
provider routes, model identities, context limits, and reasoning modes. The
model-catalog crate validates this data and embeds the shared provider logos.
Logo licenses and attribution live beside the assets.

Desktop displays providers by catalog tier and obtains models from Runtime.
Credentials determine availability. Each model's selected reasoning mode is
persisted independently. Saved selections and preferences are read as stored;
model availability and supported modes are checked when selected or used.

Core protocol adapters own provider request and response semantics. Anthropic
continuations preserve content blocks and thinking signatures. Gemini's
OpenAI-compatible adapter preserves opaque tool-call thought signatures.
Provider-specific cache parameters are emitted only for supported endpoints.

Hosted consumers obtain the catalog from their selected Core revision. Workspace
must pin a publicly reachable Core commit and compile against that exact source.
Hosted configuration and deployment policy belong to the Workspace repository.

Catalog data changes use catalog validation and directly affected checks.
Adapter changes also run their focused protocol tests. Tests cover model identity
uniqueness, route and reasoning-mode validation, tool continuation, and persistent
per-model preferences. Live provider acceptance uses separately configured
credentials and is not implied by unit tests.

Kimi Code uses `k3.1-pro`, `k3.1-pro-highspeed`, `k3`, `k3-256k`,
`kimi-for-coding` (K2.8 Preview), and `kimi-for-coding-highspeed` (K2.7 Code).
The first five expose `low`, `high`, and `max` reasoning effort; the high-speed
K2.7 model has thinking enabled without an effort selector. See the
[Kimi Code model configuration](https://www.kimi.com/code/docs/kimi-code/models.html).

Provider routes retain their own model IDs. OpenCode Go and Command Code GOAT
include the existing DeepSeek/GLM entries and the low-cost MiniMax M3.1 Flash,
MiMo V2.6 Pro/Flash, and Qwen 3.8 Flash additions. Their model IDs and endpoints
come from the [OpenCode Go](https://opencode.ai/docs/go/) and
[Command Code](https://commandcode.ai/docs/reference/cli/models) references.

Catalog output limits configure ordinary request budgets. Kimi K3.1 retains its
documented 131,072-token default; Gemini 4 Argon uses its synchronous 65,536-token
limit. Extended-output services use separate provider contracts.
