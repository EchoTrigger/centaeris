FROM ghcr.io/astral-sh/uv:0.11.6@sha256:b1e699368d24c57cda93c338a57a8c5a119009ba809305cc8e86986d4a006754 AS uv
FROM python:3.12.10-slim-bookworm@sha256:fd95fa221297a88e1cf49c55ec1828edd7c5a428187e67b5d1805692d11588db

ENV PATH="/app/.venv/bin:$PATH" \
    PADDLE_PDX_CACHE_HOME=/opt/centaeris/paddlex-cache \
    PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK=True \
    CENTAERIS_MODEL_MANIFEST=/opt/centaeris/models/manifest.json \
    HOME=/home/processor

COPY --from=uv /uv /uvx /bin/
RUN sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
        fonts-dejavu-core \
        fonts-noto-cjk \
        libgl1 \
        libglib2.0-0 \
        libgomp1 \
        libreoffice-calc \
        libreoffice-impress \
        libreoffice-writer \
    && rm -rf /var/lib/apt/lists/*

ARG PROCESSOR_DEVICE=cpu
ENV CENTAERIS_PROCESSOR_DEVICE=${PROCESSOR_DEVICE}

WORKDIR /app
COPY LICENSE /usr/share/licenses/centaeris-workspace/LICENSE
COPY pyproject.toml uv.lock ./
COPY packages/api/pyproject.toml packages/api/pyproject.toml
COPY packages/hosted_execution/pyproject.toml packages/hosted_execution/pyproject.toml
COPY packages/worker/pyproject.toml packages/worker/pyproject.toml
COPY packages/document_processor/pyproject.toml packages/document_processor/document_processor.py packages/document_processor/processor_build_extra.py packages/document_processor/
RUN --mount=type=cache,target=/root/.cache/uv \
        PROCESSOR_EXTRA="$(python packages/document_processor/processor_build_extra.py "$PROCESSOR_DEVICE")" \
    && UV_CONCURRENT_DOWNLOADS=2 uv sync --frozen --no-dev --package centaeris-document-processor --extra "$PROCESSOR_EXTRA" \
    && python packages/document_processor/document_processor.py fetch-models \
    && groupadd --gid 10001 processor \
    && useradd --uid 10001 --gid processor --create-home processor \
    && mkdir -p /data/input /data/output \
    && chown 10001:10001 /data/output \
    && chmod 0555 /data/input \
    && chmod -R a+rX,a-w /opt/centaeris/models /opt/centaeris/paddlex-cache

WORKDIR /app/packages/document_processor
USER processor:processor
ENTRYPOINT ["python", "document_processor.py"]
