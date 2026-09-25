# Provider catalog

`packages/model-catalog/centaeris_model_catalog/catalog.json` owns built-in
provider routes, model identities, context limits, and reasoning modes. The
model-catalog crate validates this data and embeds the shared provider logos.
Logo licenses and attribution live beside the assets.

Desktop displays providers by catalog tier and obtains models from Runtime.
Credentials determine availability. Each model's selected reasoning mode is
persisted independently and must belong to its supported modes. Configured
provider and model identities are validated exactly when loaded.

Core protocol adapters own provider request and response semantics. Anthropic
continuations preserve content blocks and thinking signatures. Gemini's
OpenAI-compatible adapter preserves opaque tool-call thought signatures.
Provider-specific cache parameters are emitted only for supported endpoints.

Hosted consumers obtain the catalog from their selected Core revision. Workspace
must pin a publicly reachable Core commit and compile against that exact source.
Hosted configuration and deployment policy belong to the Workspace repository.

Catalog changes require catalog validation, adapter tests, and the local release
gate. Tests cover model identity uniqueness, route and reasoning-mode validation,
tool continuation, and persistent per-model preferences. Live provider acceptance
uses separately configured credentials and is not implied by unit tests.
