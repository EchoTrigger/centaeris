#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
python perf/harness/control.py init
