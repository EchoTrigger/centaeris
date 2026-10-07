$ErrorActionPreference = "Stop"
python (Join-Path $PSScriptRoot "agent-run-authorization-gate.py")
exit $LASTEXITCODE
