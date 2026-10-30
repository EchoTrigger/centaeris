#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$workspace_root"
target_directory="${CARGO_TARGET_DIR:-$workspace_root/target}"
host="$(rustc -vV | sed -n 's/^host: //p')"
case "$host" in
  *-unknown-linux-gnu) target="${host%-gnu}-musl" ;;
  *) echo "static execution helper requires a native GNU Linux compiler host: $host" >&2; exit 64 ;;
esac

# musl-tools supplies the native musl-gcc used both by Rust's linker and the
# bundled SQLite C build. Host services keep their ordinary GNU build flags.
rustup target add "$target"
linker_variable="CARGO_TARGET_${target^^}_LINKER"
linker_variable="${linker_variable//-/_}"
(
  export CC=musl-gcc RUSTFLAGS=-Ctarget-feature=+crt-static
  export "$linker_variable=musl-gcc"
  cargo rustc --locked --release --target "$target" --target-dir "$target_directory" \
    -p hosted_execution --bin execution_agent -- -C relocation-model=static
)

helper="$target_directory/$target/release/execution_agent"
header="$(readelf -h "$helper")"
program_headers="$(readelf -l "$helper")"
dynamic_tags="$(readelf -d "$helper")"
if ! grep -Eq 'Type:[[:space:]]+EXEC' <<<"$header" \
    || grep -q 'INTERP' <<<"$program_headers" \
    || grep -q 'NEEDED' <<<"$dynamic_tags"; then
  echo "runsc requires a static non-PIE execution helper without an ELF interpreter or shared libraries" >&2
  exit 65
fi

# This is the sole verified helper artifact exported by the sandbox Docker image.
install -D -m 0755 "$helper" "$target_directory/static-helper/execution_agent"
