FROM ghcr.io/astral-sh/uv:0.11.6@sha256:b1e699368d24c57cda93c338a57a8c5a119009ba809305cc8e86986d4a006754 AS uv
FROM python:3.12.10-slim-bookworm@sha256:fd95fa221297a88e1cf49c55ec1828edd7c5a428187e67b5d1805692d11588db

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PATH="/app/.venv/bin:$PATH"

COPY --from=uv /uv /uvx /bin/
WORKDIR /app
COPY . .
COPY LICENSE /usr/share/licenses/centaeris-workspace/LICENSE
RUN --mount=type=cache,target=/root/.cache/uv uv sync --frozen --no-dev --package api
WORKDIR /app/packages/api
ENTRYPOINT ["sh", "/app/packages/api/entrypoint.sh"]
