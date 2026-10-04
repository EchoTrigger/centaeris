param([switch]$CheckOnly, [switch]$Resume, [switch]$Foreground, [switch]$FollowWithFull, [string]$SettingsPath)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$previousPythonPath = $env:PYTHONPATH
if (-not $SettingsPath) { $SettingsPath = $env:CENTAERIS_BENCH_SETTINGS }
if (-not $SettingsPath) { $SettingsPath = [Environment]::GetEnvironmentVariable('CENTAERIS_BENCH_SETTINGS', 'User') }
if (-not $SettingsPath) { $SettingsPath = Join-Path $env:LOCALAPPDATA 'Centaeris/benchmarks/settings.json' }
$settings = Get-Content -LiteralPath $SettingsPath -Raw | ConvertFrom-Json -AsHashtable
foreach ($field in @('providerId', 'model', 'credentialEnv', 'reasoningEffort', 'contextTokens', 'maxOutputTokens')) {
    if (-not $settings.ContainsKey($field)) { throw "Missing local setting: $field" }
}
$credentialEnv = $settings.credentialEnv
if ($credentialEnv -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { throw 'Invalid credential environment variable name.' }
$previousKey = [Environment]::GetEnvironmentVariable($credentialEnv, 'Process')
$previousCredentialEnv = $env:CENTAERIS_BENCH_CREDENTIAL_ENV
$previousBinary = $env:CENTAERIS_RUNTIME_BINARY
Push-Location $repoRoot
try {
    if (-not $env:CENTAERIS_RUNTIME_BINARY) {
        $env:CENTAERIS_RUNTIME_BINARY = Join-Path $repoRoot 'target/harbor-bookworm/debug/centaeris-runtime'
    }
    if (-not (Test-Path -LiteralPath $env:CENTAERIS_RUNTIME_BINARY -PathType Leaf)) {
        throw 'Build the Bookworm Linux runtime first; see scripts/harbor/README.md.'
    }
    Get-Command harbor -ErrorAction Stop | Out-Null
    $dockerKind = docker info --format '{{.OSType}}'
    if ($LASTEXITCODE -ne 0 -or $dockerKind -ne 'linux') {
        throw 'A running Linux Docker engine is required.'
    }
    $adapterPath = Join-Path $repoRoot 'scripts/harbor'
    $env:PYTHONPATH = if ($previousPythonPath) { "$adapterPath;$previousPythonPath" } else { $adapterPath }
    $jobPath = Join-Path $repoRoot 'jobs/centaeris-tb21-five-tasks-pass5'
    $controlPath = Join-Path $repoRoot 'jobs/control/centaeris-tb21-five-tasks-pass5'
    $workerPath = Join-Path $controlPath 'worker.json'
    if ((Test-Path -LiteralPath (Join-Path $jobPath 'config.json'))) { $Resume = $true }
    $settingsDirectory = Split-Path (Resolve-Path -LiteralPath $SettingsPath).Path -Parent
    $resolvedConfigs = @{}
    foreach ($name in @('five-tasks', 'full-tasks')) {
        $config = Get-Content -LiteralPath (Join-Path $adapterPath "$name.json") -Raw | ConvertFrom-Json -AsHashtable
        $config.agents[0].model_name = $settings.model
        $config.agents[0].kwargs = @{
            provider_id = $settings.providerId; credential_env = $credentialEnv
            reasoning_effort = $settings.reasoningEffort
            context_tokens = $settings.contextTokens; max_output_tokens = $settings.maxOutputTokens
        }
        $resolvedConfigs[$name] = Join-Path $settingsDirectory "resolved-$name.json"
        $config | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath $resolvedConfigs[$name]
    }
    $env:CENTAERIS_BENCH_CREDENTIAL_ENV = $credentialEnv
    Write-Host "Terminal-Bench 2.1: 5 tasks x 5 attempts, concurrency 2; $($settings.providerId) / $($settings.model), $($settings.reasoningEffort), $($settings.contextTokens)/$($settings.maxOutputTokens) tokens."
    if ($CheckOnly) {
        Write-Host 'Local prerequisites available. No key read and no model requests made.'
        return
    }
    $key = $previousKey
    if (-not $key) { $key = [Environment]::GetEnvironmentVariable($credentialEnv, 'User') }
    if (-not $key) {
        $secureKey = Read-Host "API key ($credentialEnv)" -AsSecureString
        $key = [System.Net.NetworkCredential]::new('', $secureKey).Password
    }
    if (-not $key) { throw 'The configured API key is empty.' }
    [Environment]::SetEnvironmentVariable($credentialEnv, $key, 'Process')
    if (-not $Foreground) {
        if (Test-Path -LiteralPath $workerPath) {
            $record = Get-Content -LiteralPath $workerPath -Raw | ConvertFrom-Json
            $runningWorker = Get-CimInstance Win32_Process -Filter "ProcessId=$($record.processId)"
            if ($runningWorker -and $runningWorker.CommandLine.Contains($PSCommandPath)) {
                throw "This job already has a background worker: $($record.processId)"
            }
        }
        New-Item -ItemType Directory -Path $controlPath -Force | Out-Null
        Remove-Item -LiteralPath (Join-Path $controlPath 'finished.json') -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $controlPath 'review.json') -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $controlPath 'phase.json') -ErrorAction SilentlyContinue
        $workerArgs = @('-NoProfile', '-File', ('"' + $PSCommandPath + '"'), '-Foreground', '-SettingsPath', ('"' + (Resolve-Path -LiteralPath $SettingsPath).Path + '"'))
        if ($Resume) { $workerArgs += '-Resume' }
        if ($FollowWithFull) { $workerArgs += '-FollowWithFull' }
        $worker = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList $workerArgs `
            -WindowStyle Hidden -WorkingDirectory $repoRoot -PassThru `
            -RedirectStandardOutput (Join-Path $controlPath 'stdout.log') `
            -RedirectStandardError (Join-Path $controlPath 'stderr.log')
        @{ processId = $worker.Id; startedAt = (Get-Date).ToUniversalTime().ToString('o') } |
            ConvertTo-Json | Set-Content -LiteralPath $workerPath
        Write-Host "Background worker started: $($worker.Id). This terminal can close."
        Write-Host "Logs: $controlPath"
        return
    }
    if ($Resume) {
        $backupPath = Join-Path $repoRoot ("jobs/backups/centaeris-tb21-before-resume-" + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path (Split-Path $backupPath -Parent) -Force | Out-Null
        Copy-Item -LiteralPath $jobPath -Destination $backupPath -Recurse
        harbor jobs resume -p $jobPath -f CancelledError
    }
    else { harbor run -c $resolvedConfigs['five-tasks'] }
    $harborExit = $LASTEXITCODE
    New-Item -ItemType Directory -Path $controlPath -Force | Out-Null
    if ($harborExit -eq 0 -and $FollowWithFull) {
        @{ stage = 'awaitingReview'; pilotExitCode = $harborExit; finishedAt = (Get-Date).ToUniversalTime().ToString('o') } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $controlPath 'phase.json')
        # Hold the inherited key in memory until the requested log review finishes.
        # The review file selects a fixed configuration, never an arbitrary command.
        $reviewPath = Join-Path $controlPath 'review.json'
        $reviewDeadline = (Get-Date).AddHours(6)
        while (-not (Test-Path -LiteralPath $reviewPath)) {
            if ((Get-Date) -gt $reviewDeadline) { throw 'Timed out waiting for pilot log review.' }
            Start-Sleep -Milliseconds 250
        }
        $review = Get-Content -LiteralPath $reviewPath -Raw | ConvertFrom-Json
        if ($review.decision -eq 'runFull') {
            @{ stage = 'fullRunning'; startedAt = (Get-Date).ToUniversalTime().ToString('o') } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $controlPath 'phase.json')
            harbor run -c $resolvedConfigs['full-tasks']
            $harborExit = $LASTEXITCODE
        }
        elseif ($review.decision -ne 'stop') { throw 'Unsupported pilot review decision.' }
    }
    @{ exitCode = $harborExit; finishedAt = (Get-Date).ToUniversalTime().ToString('o') } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $controlPath 'finished.json')
    if ($harborExit -ne 0) { throw "Harbor exited with code $harborExit" }
}
finally {
    $env:PYTHONPATH = $previousPythonPath
    [Environment]::SetEnvironmentVariable($credentialEnv, $previousKey, 'Process')
    $env:CENTAERIS_BENCH_CREDENTIAL_ENV = $previousCredentialEnv
    $env:CENTAERIS_RUNTIME_BINARY = $previousBinary
    Pop-Location
}
