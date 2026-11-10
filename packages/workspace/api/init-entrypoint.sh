#!/bin/sh
set -eu

python manage.py initialize_plugin_catalog
python manage.py migrate --noinput
python manage.py bootstrap_superadmin
