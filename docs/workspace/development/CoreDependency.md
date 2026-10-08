# Developing with the shared Core source

All products live in one checkout. The four Core dependencies are local paths
in the root Cargo workspace; no Core Git pin, sibling checkout or development
patch is used. Cargo.lock, pnpm-lock.yaml and uv.lock belong to the root.

Run commands from the repository root, regardless of the scripts/workspace
location. Its Python tools deliberately resolve the monorepo root two levels
above their file; source-discovery tests validate that root and all four Cargo
package identities, including paths with spaces.

```sh
node scripts/workspace/core-source.mjs
python scripts/ci.py Rust
python scripts/workspace/ci.py --stage Rust
python scripts/workspace/ci.py --stage Web
python scripts/workspace/ci.py --stage Python
```

The Python gate requires TEST_POSTGRES_* to identify a disposable database.
The default hosted CI entry runs all retained hosted checks. Use python3 on
systems without a python alias. No uv installation is performed by local Rust
or Desktop/TUI build entries.

```sh
python scripts/products.py build runtime
python scripts/products.py build tui
python scripts/products.py build desktop
python scripts/products.py build runtime-server
python scripts/products.py build hosted-execution
python scripts/products.py build web
python scripts/workspace/build.py
```

Hosted image builds require clean tracked source and derive the full revision
from git HEAD. Compose keeps the centaeris-workspace project and existing service,
volume and container data identities. Root context is filtered by .dockerignore;
each Dockerfile selects its own source and required workspace manifests.

[Source unification](../../development/SourceUnification.md) records provenance,
component boundaries and the independently selectable gates. Original repository
history remains available at the full SHA in the snapshot manifest.
