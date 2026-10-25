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
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked cargo build --release --locked -p runtime_server

FROM ghcr.io/astral-sh/uv:0.11.6@sha256:b1e699368d24c57cda93c338a57a8c5a119009ba809305cc8e86986d4a006754 AS uv
FROM python:3.12.10-slim-bookworm@sha256:fd95fa221297a88e1cf49c55ec1828edd7c5a428187e67b5d1805692d11588db AS adapter-deps

COPY --from=uv /uv /uvx /bin/
ENV UV_PROJECT_ENVIRONMENT=/opt/centaeris/venv
WORKDIR /src
COPY pyproject.toml uv.lock .python-version ./
COPY packages/api/pyproject.toml packages/api/pyproject.toml
COPY packages/hosted_execution/pyproject.toml packages/hosted_execution/pyproject.toml
COPY packages/worker/pyproject.toml packages/worker/pyproject.toml
COPY packages/document_processor/pyproject.toml packages/document_processor/pyproject.toml
COPY packages/e2b_adapter/pyproject.toml packages/e2b_adapter/pyproject.toml
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --package centaeris-e2b-adapter --no-install-workspace

FROM python:3.12.10-slim-bookworm@sha256:fd95fa221297a88e1cf49c55ec1828edd7c5a428187e67b5d1805692d11588db

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
COPY --from=adapter-deps /opt/centaeris/venv /opt/centaeris/venv
COPY packages/e2b_adapter/centaeris_e2b_adapter /opt/centaeris/e2b-adapter/centaeris_e2b_adapter

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/centaeris-runtime

COPY --from=build /src/target/release/runtime_server /usr/local/bin/runtime_server
COPY LICENSE LICENSE-APACHE LICENSING.md COPYRIGHT /usr/share/licenses/centaeris-workspace/
COPY packages/model-catalog/centaeris_model_catalog/logos/LICENSE /usr/share/licenses/centaeris-workspace/packages/model-catalog/centaeris_model_catalog/logos/LICENSE
COPY skills/system /opt/centaeris/system-skills

CMD ["runtime_server"]
