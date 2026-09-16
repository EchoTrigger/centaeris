set -eu
umask 077
operation=$1
shift
case "$operation" in
  install)
    source_path=$1
    digest=$2
    base="$HOME/.local/share/centaeris"
    destination="$base/bin/$digest"
    mkdir -p "$destination" "$base/workspaces/default"
    if [ ! -f "$destination/centaeris-runtime" ]; then
      temporary=$(mktemp "$destination/.runtime.XXXXXX")
      trap 'rm -f "$temporary"' EXIT
      cp -- "$source_path" "$temporary"
      actual=$(sha256sum "$temporary" | cut -d ' ' -f 1)
      [ "$actual" = "$digest" ] || exit 1
      chmod 500 "$temporary"
      mv -- "$temporary" "$destination/centaeris-runtime"
    fi
    printf '%s\n' "$destination/centaeris-runtime" "$base/workspaces/default" "$HOME/.centaeris"
    ;;
  start|endpoint|connect)
    binary=$1
    workspace=$2
    data=$3
    case "$operation" in
      start)
        identity=$(printf '%s' "$data" | sha256sum | cut -c 1-16)
        exec systemd-run --user --collect "--unit=centaeris-runtime-$identity" \
          --property=Delegate=yes --property=KillMode=control-group \
          "--working-directory=$workspace" "--setenv=CENTAERIS_DESKTOP_DATA_DIR=$data" \
          /usr/bin/env -u USERPROFILE "$binary" --runtime-server
        ;;
      endpoint) mode=--runtime-server-endpoint ;;
      connect) mode=--runtime-server-connect ;;
    esac
    exec /usr/bin/env -u USERPROFILE "CENTAERIS_DESKTOP_DATA_DIR=$data" \
      /usr/bin/env -C "$workspace" "$binary" "$mode"
    ;;
  *) printf '%s\n' 'unsupported WSL bootstrap operation' >&2; exit 2 ;;
esac
