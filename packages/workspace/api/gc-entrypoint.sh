#!/bin/sh
set -eu

while true; do
  if ! python manage.py gc_deleted_resources; then
    printf '%s\n' 'GC failed; waiting until the next scheduled attempt' >&2
  fi
  sleep 86400
done
