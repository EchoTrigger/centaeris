param(
    [string]$SystemSkillsSource = $env:CENTAERIS_SYSTEM_SKILLS_SOURCE
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$uiDir = Join-Path $repoRoot "packages/ui"
$electronDir = Join-Path $repoRoot "packages/desktop"
$releaseRuntime = Join-Path $repoRoot "target/release/centaeris-runtime.exe"
$desktopDist = Join-Path $electronDir "dist/Centaeris Desktop"
$packagedRuntime = Join-Path $desktopDist "resources/bin/centaeris-runtime.exe"
$centaerisExe = Join-Path $desktopDist "Centaeris Desktop.exe"
$uiIndex = Join-Path $repoRoot "packages/ui/dist/index.html"

# Some terminals can have COMSPEC changed; npm run may fail if it is not cmd.exe.
$env:ComSpec = "C:\Windows\System32\cmd.exe"

function Assert-CommandAvailable {
    param(
        [Parameter(Mandatory = $true)][string]$Command
    )
    if ($null -eq (Get-Command $Command -ErrorAction SilentlyContinue)) {
        throw "Required command is not available on PATH: $Command"
    }
}

function Assert-PathExists {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Label
    )
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "$Label does not exist: $Path"
    }
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )
    Write-Host "[desktop-build] $Name" -ForegroundColor Cyan
    Push-Location $WorkingDirectory
    try {
        & $FilePath @Arguments
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            throw "$Name failed with exit code $exitCode"
        }
    } finally {
        Pop-Location
    }
}

function Assert-DistNotRunning {
    if (-not (Test-Path -LiteralPath $centaerisExe)) {
        return
    }
    $runningDistProcesses = Get-Process -Name "Centaeris Desktop" -ErrorAction SilentlyContinue |
        Where-Object {
            try {
                [string]::Equals($_.Path, $centaerisExe, [System.StringComparison]::OrdinalIgnoreCase)
            } catch {
                $false
            }
        }
    if ($runningDistProcesses) {
        throw "Close the packaged Centaeris Desktop.exe before rebuilding: $centaerisExe"
    }
}

function Assert-RuntimeHashesMatch {
    $releaseHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $releaseRuntime).Hash
    $packagedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $packagedRuntime).Hash
    if ($releaseHash -ne $packagedHash) {
        throw "Packaged runtime hash does not match release runtime. Release=$releaseHash Packaged=$packagedHash"
    }
}

function Assert-RuntimeFreshForRustSources {
    Invoke-Checked "runtime freshness gate" "node.exe" @((Join-Path $electronDir "scripts/ensure-runtime.mjs"), "--check") $repoRoot
}

Assert-CommandAvailable "npm.cmd"
Assert-CommandAvailable "node.exe"
Assert-CommandAvailable "cargo.exe"
Assert-DistNotRunning

if ($SystemSkillsSource) {
    $resolvedSystemSkillsSource = (Resolve-Path -LiteralPath $SystemSkillsSource).Path
    $env:CENTAERIS_SYSTEM_SKILLS_SOURCE = $resolvedSystemSkillsSource
} else {
    Remove-Item Env:CENTAERIS_SYSTEM_SKILLS_SOURCE -ErrorAction SilentlyContinue
}

Invoke-Checked "ui production build" "npm.cmd" @("run", "build") $uiDir
Invoke-Checked "electron release runtime and desktop dist build" "npm.cmd" @("run", "build") $electronDir

Assert-PathExists $uiIndex "UI dist index"
Assert-PathExists $releaseRuntime "Rust release runtime"
Assert-PathExists $packagedRuntime "Packaged Rust runtime"
Assert-PathExists $centaerisExe "Packaged Centaeris Desktop executable"
Assert-RuntimeFreshForRustSources
Assert-RuntimeHashesMatch

Write-Host "[desktop-build] complete" -ForegroundColor Green
Write-Host "Executable: $centaerisExe"
