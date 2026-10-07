param([switch]$SkipFrontendTests)
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$arguments = @((Join-Path $PSScriptRoot "ci.py"))
if ($SkipFrontendTests) { $arguments += "--skip-frontend-tests" }
python @arguments
exit $LASTEXITCODE
