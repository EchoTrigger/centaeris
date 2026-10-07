#!/usr/bin/env bash
# Reproducible Windows load-generator build; binary stays outside Git.
set -euo pipefail
cd "$(dirname "$0")"
source versions.env
mkdir -p bin
output="$(pwd -W 2>/dev/null || pwd)/bin"
MSYS_NO_PATHCONV=1 docker run --rm \
  --mount "type=bind,source=$output,target=/out" \
  -e "K6_VERSION=$K6_VERSION" -e "XK6_VERSION=$XK6_VERSION" \
  -e "XK6_SSE_MODULE=$XK6_SSE_MODULE" -e "XK6_SSE_VERSION=$XK6_SSE_VERSION" \
  "$GO_IMAGE" bash -ceu '
    go install go.k6.io/xk6@"$XK6_VERSION"
    GOOS=windows GOARCH=amd64 CGO_ENABLED=0 "$(go env GOPATH)/bin/xk6" build "$K6_VERSION" \
      --with "$XK6_SSE_MODULE@$XK6_SSE_VERSION" --output /out/k6.exe
  '
