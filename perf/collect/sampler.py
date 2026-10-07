"""Use the isolated controller for ownership checks and fail-fast sampling."""
from pathlib import Path
import subprocess
import sys

if __name__ == '__main__':
    controller = Path(__file__).resolve().parents[1] / 'harness/control.py'
    raise SystemExit(subprocess.call([sys.executable, str(controller), 'sample', *sys.argv[1:]]))
