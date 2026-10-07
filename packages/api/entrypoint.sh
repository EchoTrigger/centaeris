#!/bin/sh
set -eu

: "${API_WORKERS:?Missing required environment variable: API_WORKERS}"
case "$API_WORKERS" in
  *[!0-9]*)
    echo "API_WORKERS must be an integer from 1 to 64" >&2
    exit 1
    ;;
esac
if ! [ "$API_WORKERS" -ge 1 ] 2>/dev/null || ! [ "$API_WORKERS" -le 64 ] 2>/dev/null; then
  echo "API_WORKERS must be an integer from 1 to 64" >&2
  exit 1
fi

exec uvicorn api.asgi:application \
  --host 0.0.0.0 \
  --port 8000 \
  --workers "$API_WORKERS"
