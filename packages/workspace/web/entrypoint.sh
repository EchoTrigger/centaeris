#!/bin/sh
set -eu

: "${API_BASE_URL:?API_BASE_URL is required}"
case "$API_BASE_URL" in
  /|http://*|https://*) ;;
  *) echo "API_BASE_URL must use http or https" >&2; exit 1 ;;
esac
case "$API_BASE_URL" in
  *\"*|*\\*) echo "API_BASE_URL contains unsupported characters" >&2; exit 1 ;;
esac

printf '{"apiBaseUrl":"%s"}\n' "$API_BASE_URL" > /usr/share/nginx/html/config.json
: "${UPLOAD_BODY_MAX_BYTES:?UPLOAD_BODY_MAX_BYTES is required}"
case "$UPLOAD_BODY_MAX_BYTES" in
  ''|*[!0-9]*) echo "UPLOAD_BODY_MAX_BYTES must be a positive bounded integer" >&2; exit 1 ;;
esac
if [ "${#UPLOAD_BODY_MAX_BYTES}" -gt 19 ] || [ "$UPLOAD_BODY_MAX_BYTES" -le 0 ] || [ "$UPLOAD_BODY_MAX_BYTES" -gt 4611686018427387903 ]; then
  echo "UPLOAD_BODY_MAX_BYTES must be between 1 and 4611686018427387903" >&2
  exit 1
fi
envsubst '${UPLOAD_BODY_MAX_BYTES}' < /etc/nginx/upload-boundary.conf.template > /etc/nginx/conf.d/default.conf
exec nginx -g 'daemon off;'
