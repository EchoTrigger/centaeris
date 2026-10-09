# Centaeris Model Catalog

`packages/model-catalog` (`centaeris-model-catalog`) is the static provider and
model catalog shared by local and hosted products. It ships `ModelCatalog`
Rust types plus the actual catalog data and logos:

- `centaeris_model_catalog/catalog.json` — the schema-`centaeris.model_catalog.v1`
  catalog of providers, models, credentials, and API routing.
- `centaeris_model_catalog/logos/` — bundled provider logo SVGs and their
  license notice.

This package only defines catalog shape and ships the frozen data; it does not
perform provider HTTP calls, admission, or credential storage. Those live in
`packages/runtime` (Local Runtime) and `packages/runtime_server` (hosted).

See [Provider catalog](../../docs/reference/ProviderCatalog.md) for model
identities, routing, and reasoning-effort preferences.
