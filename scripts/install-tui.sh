#!/bin/sh
set -eu

RELEASE="${CENTAERIS_TUI_RELEASE:-latest}"
TUI_HOME="${CENTAERIS_TUI_HOME:-$HOME/.centaeris/tui}"
STANDALONE_ROOT="$TUI_HOME/packages/standalone"
RELEASES_DIR="$STANDALONE_ROOT/releases"
CURRENT_LINK="$STANDALONE_ROOT/current"
BIN_DIR="$STANDALONE_ROOT/bin"
BIN_PATH="$BIN_DIR/centa"
REPO_NAME="EchoTrigger/Centaeris"

step() { printf '==> %s\n' "$1"; }

fetch() {
  url="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -q -O - "$url"
    return
  fi
  echo "curl or wget is required to install Centaeris TUI." >&2
  exit 1
}

download() {
  url="$1"
  output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -q -O "$output" "$url"
    return
  fi
  echo "curl or wget is required to install Centaeris TUI." >&2
  exit 1
}

# Select by the TUI asset, independent of other products' release dates.
resolve_version() {
  requested="$1"
  if [ "$requested" != "latest" ]; then
    printf '%s\n' "$requested"
    return
  fi
  page=1
  while :; do
    release_page="$(fetch "https://api.github.com/repos/$REPO_NAME/releases?per_page=100&page=$page")"
    selection="$(printf '%s\n' "$release_page" | awk -v asset="$ASSET_NAME" '
      function assets_array(object, tail, start, i, c, depth, quoted, escaped) {
        if (!match(object, /"assets"[[:space:]]*:/)) return ""
        tail=substr(object, RSTART+RLENGTH); start=index(tail, "[")
        if (!start) return ""
        for (i=start; i<=length(tail); i++) {
          c=substr(tail,i,1)
          if (quoted) {
            if (escaped) escaped=0
            else if (c=="\\") escaped=1
            else if (c=="\"") quoted=0
          } else if (c=="\"") quoted=1
          else if (c=="[") depth++
          else if (c=="]" && --depth==0) return substr(tail,start,i-start+1)
        }
        return ""
      }
      function release_tag(object, tag, assets, name) {
        if (object ~ /"draft"[[:space:]]*:[[:space:]]*true/ || object ~ /"prerelease"[[:space:]]*:[[:space:]]*true/) return ""
        if (!match(object, /"tag_name"[[:space:]]*:[[:space:]]*"[^"\\]*"/)) return ""
        tag=substr(object,RSTART,RLENGTH)
        sub(/^"tag_name"[[:space:]]*:[[:space:]]*"/,"",tag); sub(/"$/,"",tag)
        assets=assets_array(object)
        while (match(assets, /"name"[[:space:]]*:[[:space:]]*"[^"\\]*"/)) {
          name=substr(assets,RSTART,RLENGTH)
          assets=substr(assets,RSTART+RLENGTH)
          sub(/^"name"[[:space:]]*:[[:space:]]*"/,"",name); sub(/"$/,"",name)
          if (name==asset) return tag
        }
        return ""
      }
      {
        for (i=1;i<=length($0);i++) {
          c=substr($0,i,1)
          if (!quoted && c=="{") { if (depth==0) object=""; depth++ }
          if (depth>0) object=object c
          if (quoted) {
            if (escaped) escaped=0
            else if (c=="\\") escaped=1
            else if (c=="\"") quoted=0
          } else if (c=="\"") quoted=1
          else if (c=="}" && --depth==0) {
            count++; tag=release_tag(object)
            if (chosen=="" && tag!="") chosen=tag
          }
        }
      }
      END { print count+0; print chosen }
    ')"
    count="$(printf '%s\n' "$selection" | sed -n '1p')"
    version="$(printf '%s\n' "$selection" | sed -n '2p')"
    if [ -n "$version" ]; then printf '%s\n' "$version"; return; fi
    if [ "$count" -lt 100 ]; then
      echo "No stable TUI release provides $ASSET_NAME (published platform support is defined by the release assets)." >&2
      return 1
    fi
    page=$((page + 1))
  done
}

if [ "$(uname -s)" = "Darwin" ]; then
  ASSET_NAME="centaeris-darwin-arm64.zip"
  EXTRACT="unzip -qo"
else
  ASSET_NAME="centaeris-linux-x64.tar.gz"
  EXTRACT="tar -xzf"
fi

version="$(resolve_version "$RELEASE")"
if [ -z "$version" ]; then
  echo "Failed to resolve release version." >&2
  exit 1
fi

step "Centaeris TUI installer: version $version"

release_json="$(fetch "https://api.github.com/repos/$REPO_NAME/releases/tags/$version")"

digest="$(printf '%s\n' "$release_json" | grep -A 20 "\"name\": \"$ASSET_NAME\"" | grep '"digest"' | head -n 1 | sed 's/.*"digest": *"\([^"]*\)".*/\1/')"
if [ -z "$digest" ]; then
  echo "No digest found for $ASSET_NAME in release $version." >&2
  exit 1
fi
expected_hash="${digest#sha256:}"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
archive="$tmp_dir/$ASSET_NAME"
download_url="$(printf '%s\n' "$release_json" | grep -A 20 "\"name\": \"$ASSET_NAME\"" | grep '"browser_download_url"' | head -n 1 | sed 's/.*"browser_download_url": *"\([^"]*\)".*/\1/')"
if [ -z "$download_url" ]; then
  echo "No download URL found for $ASSET_NAME." >&2
  exit 1
fi

step "Downloading $ASSET_NAME"
download "$download_url" "$archive"
actual_hash="$(sha256sum "$archive" | awk '{print $1}')"
if [ "$actual_hash" != "$expected_hash" ]; then
  echo "Digest mismatch: expected $expected_hash, got $actual_hash" >&2
  exit 1
fi
step "Digest verified"

install_dir="$RELEASES_DIR/$version"
mkdir -p "$install_dir"
step "Extracting to $install_dir"
$EXTRACT "$archive" -C "$install_dir"

manifest="$install_dir/centaeris-package.json"
if [ ! -f "$manifest" ]; then
  echo "Package manifest missing: $manifest" >&2
  exit 1
fi
if ! grep -q '"schema": "centaeris-package.v1"' "$manifest"; then
  echo "Unsupported package manifest schema." >&2
  exit 1
fi
path_list="$(printf '%s\n' centa centaeris-runtime)"
ok=1
while IFS= read -r path; do
  file_size="$(wc -c < "$install_dir/$path" | tr -d ' ')"
  expected_file_size="$(grep -A 3 "\"path\": \"$path\"" "$manifest" | grep '"sizeBytes"' | head -n 1 | sed 's/[^0-9]*\([0-9][0-9]*\).*/\1/')"
  if [ "$file_size" != "$expected_file_size" ]; then
    echo "Package file size mismatch: $path" >&2
    ok=0
  fi
  file_hash="$(sha256sum "$install_dir/$path" | awk '{print $1}')"
  expected_file_hash="$(grep -A 3 "\"path\": \"$path\"" "$manifest" | grep '"sha256"' | head -n 1 | sed 's/.*"sha256": *"\([^"]*\)".*/\1/')"
  if [ "$file_hash" != "$expected_file_hash" ]; then
    echo "Package file digest mismatch: $path" >&2
    ok=0
  fi
done <<EOF
$path_list
EOF
if [ "$ok" != "1" ]; then
  exit 1
fi
step "Package manifest verified"

if [ -L "$CURRENT_LINK" ] || [ -e "$CURRENT_LINK" ]; then
  rm -rf "$CURRENT_LINK"
fi
ln -s "$install_dir" "$CURRENT_LINK"

mkdir -p "$BIN_DIR"
cp "$CURRENT_LINK/centa" "$BIN_PATH"
if [ -f "$CURRENT_LINK/centaeris-runtime" ]; then
  cp "$CURRENT_LINK/centaeris-runtime" "$BIN_DIR/centaeris-runtime"
fi
step "Installed: $BIN_PATH"
step "Current version: $version"
step "Run: $BIN_PATH"
