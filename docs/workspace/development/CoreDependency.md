# Developing with Core

Workspace can be cloned into any directory on Windows, macOS or Linux. Rust
dependencies use the public Core repository and an exact Git revision; Cargo
fetches the source automatically. No adjacent Core checkout is required.
Use the checked-in Rust toolchain, Node 22.21.0 and Python 3.12.10 with uv.
On macOS/Linux use `python3` where these examples say `python` if needed.

```sh
cargo check --workspace --locked
node scripts/verify-core-checkout.mjs
python scripts/ci.py
```

The full gate requires a disposable PostgreSQL instance configured through
`TEST_POSTGRES_*`; see the release gate. It does not require PowerShell.
`scripts/ci.ps1` remains a wrapper for existing Windows commands.

`scripts/core-source.mjs` finds the four Core crates through locked Cargo metadata
and verifies their source identity. Contract generation, Web parity/fixture tests
and performance evaluation use this same resolver. Cargo may download source on
the first invocation. Do not edit the Cargo Git cache.

All Docker images build from the Workspace context. Rust builds fetch the same
locked Git dependency. Docker builds need Git network access on a cold cache;
they do not include a developer's local Core override or require a host checkout.

## Update the dependency

After Core's required CI passes, set `core-revision.txt` to its publicly fetchable
full SHA. Then run:

```sh
python scripts/core-pin.py --sync
cargo check --workspace
python scripts/core-pin.py
python scripts/ci.py
```

Review and commit the pin, Cargo manifest/lockfile and `.env.example` together.
The sync command updates only derived manifest/example metadata; Cargo updates
the lockfile. The gate rejects drift, floating references and path dependencies.
Update an existing private `.env` Core revision before building images, so their
revision labels describe the actual source. No binary release is required.

## Explicit local co-development

Use a dedicated development checkout when changing Core and Workspace together.
The Core directory may have any name or location:

```sh
node scripts/core-dev.mjs /absolute/path/to/core-checkout
export CENTAERIS_CORE_PATH=/absolute/path/to/core-checkout
cargo check --workspace
npm run test:unit --workspace packages/web
```

In PowerShell set `$env:CENTAERIS_CORE_PATH` instead of `export`. The helper
creates an ignored `.cargo/config.toml` with four Cargo Git-source patches and
refuses to overwrite an existing config. It does not copy Core code. The explicit
environment path must agree with every resolved local crate; mixed sources fail.

Cargo changes `Cargo.lock` when applying local patches. Do not commit that local
lockfile. To return to pinned mode, remove only this generated local config, unset
`CENTAERIS_CORE_PATH`, run `cargo check --workspace`, and verify the resulting
lockfile with `python scripts/core-pin.py`. Normal release/CI gates reject local
overrides even when their HEAD happens to match the public pin. Docker continues
to build the committed Git dependency; local co-development is for targeted
source checks, not a substitute for release acceptance.
