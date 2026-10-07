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
exec nginx -g 'daemon off;'
