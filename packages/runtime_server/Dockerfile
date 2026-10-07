FROM rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY packages/runtime/Cargo.toml packages/runtime/Cargo.toml
COPY packages/runtime/src/lib.rs packages/runtime/src/lib.rs
COPY packages/tui/Cargo.toml packages/tui/Cargo.toml
COPY packages/core packages/core
COPY packages/model-catalog packages/model-catalog
COPY packages/mcp packages/mcp
COPY packages/runtime_sqlite packages/runtime_sqlite
COPY packages/hosted_execution packages/hosted_execution
COPY packages/runtime_server packages/runtime_server
COPY packages/api/app_core/contracts packages/api/app_core/contracts
RUN --mount=type=cache,target=/usr/local/cargo/registry cargo build --release --locked -p runtime_server

FROM debian:12.11-slim@sha256:b1a741487078b369e78119849663d7f1a5341ef2768798f7b7406c4240f86aef

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/centaeris-runtime

COPY --from=build /src/target/release/runtime_server /usr/local/bin/runtime_server
COPY LICENSE /usr/share/licenses/centaeris-workspace/LICENSE
COPY skills/system /opt/centaeris/system-skills

CMD ["runtime_server"]
