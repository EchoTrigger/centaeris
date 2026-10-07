from pathlib import Path
import subprocess


PLUGIN_ROOT = Path("/opt/centaeris/plugins/banana")
COMMAND = "banana-tool"


def main() -> None:
    manifest = PLUGIN_ROOT / ".centaeris-plugin/plugin.json"
    assert manifest.is_file()
    assert '"name": "banana"' in manifest.read_text(encoding="utf-8")
    result = subprocess.run(
        [COMMAND, "--self-check"], capture_output=True, text=True, timeout=30, check=False
    )
    assert result.returncode == 0, result.stderr or result.stdout
    try:
        (PLUGIN_ROOT / "mutation.txt").write_text("forbidden", encoding="utf-8")
    except OSError:
        pass
    else:
        raise AssertionError("activated Plugin package is writable")
    print("synthetic extension smoke: ok")


if __name__ == "__main__":
    main()
