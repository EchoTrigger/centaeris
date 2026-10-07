#!/bin/sh
set -eu

while true; do
  python manage.py gc_deleted_resources
  sleep 86400
done
