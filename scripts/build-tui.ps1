param(
    [string]$SystemSkillsSource = $env:CENTAERIS_SYSTEM_SKILLS_SOURCE
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$tuiDir = Join-Path $repoRoot "packages/tui"
$tuiReleaseTarget = Join-Path $repoRoot "target/tui-release"
$releaseTarget = Join-Path $repoRoot "target/release"
$tuiBinary = Join-Path $tuiReleaseTarget "centa.exe"
$runtimeBinary = Join-Path $releaseTarget "centaeris-runtime.exe"
$distRoot = Join-Path $tuiDir "dist/centaeris"

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
    Write-Host "[tui-build] $Name" -ForegroundColor Cyan
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

function Get-Version {
    $cargoToml = Get-Content -Raw -LiteralPath (Join-Path $repoRoot "Cargo.toml")
    if ($cargoToml -match 'version\s*=\s*"([^"]+)"') {
        return $Matches[1]
    }
    throw "Cannot read workspace version from root Cargo.toml"
}

function Get-FileSha256 {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-SystemSkillsBundle {
    param(
        [Parameter(Mandatory = $true)][string]$Path
    )
    $inspector = @'
import { inspectSystemSkillsBundle } from "./packages/desktop/src/systemSkills.mjs";
try {
  process.stdout.write(JSON.stringify(await inspectSystemSkillsBundle(process.argv[1])));
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
'@
    Push-Location $repoRoot
    try {
        $json = & node.exe --input-type=module --eval $inspector $Path
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            throw "System Skills bundle validation failed with exit code $exitCode"
        }
        return ($json | ConvertFrom-Json)
    } finally {
        Pop-Location
    }
}

function Assert-SystemSkillLicenses {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][object]$Bundle
    )
    foreach ($skillName in $Bundle.skillNames) {
        $files = @(Get-ChildItem -LiteralPath (Join-Path $Root $skillName) -File)
        $hasNotice = $null -ne ($files | Where-Object { $_.Name -ceq "NOTICE" } | Select-Object -First 1)
        $hasLicense = $null -ne ($files | Where-Object { $_.Name -match '^license(?:\..+)?$' } | Select-Object -First 1)
        if ($hasNotice -and -not $hasLicense) {
            throw "Bundled System Skill NOTICE has no matching license file"
        }
    }
}

if (-not $IsWindows) {
    throw "scripts/build-tui.ps1 currently supports Windows only"
}
Assert-CommandAvailable "cargo.exe"
Assert-CommandAvailable "node.exe"

$resolvedSystemSkillsSource = $null
$systemSkillsBundle = $null
if (-not [string]::IsNullOrWhiteSpace($SystemSkillsSource)) {
    $resolvedSystemSkillsSource = (Resolve-Path -LiteralPath $SystemSkillsSource).Path
    $systemSkillsBundle = Get-SystemSkillsBundle $resolvedSystemSkillsSource
    Assert-SystemSkillLicenses $resolvedSystemSkillsSource $systemSkillsBundle
}

Invoke-Checked "tui release build" "cargo.exe" @(
    "build",
    "--profile",
    "tui-release",
    "--locked",
    "--manifest-path",
    (Join-Path $tuiDir "Cargo.toml")
) $repoRoot

Invoke-Checked "runtime release build" "cargo.exe" @(
    "build",
    "--release",
    "--locked",
    "--manifest-path",
    (Join-Path $repoRoot "packages/runtime/Cargo.toml")
) $repoRoot

Assert-PathExists $tuiBinary "TUI release binary"
Assert-PathExists $runtimeBinary "Rust release runtime"

if (Test-Path -LiteralPath $distRoot) {
    Remove-Item -LiteralPath $distRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $distRoot -Force | Out-Null

$tuiName = "centa.exe"
$runtimeName = "centaeris-runtime.exe"
Copy-Item -LiteralPath $tuiBinary -Destination (Join-Path $distRoot $tuiName)
Copy-Item -LiteralPath $runtimeBinary -Destination (Join-Path $distRoot $runtimeName)
Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination (Join-Path $distRoot "LICENSE.centaeris.txt")
Copy-Item -LiteralPath (Join-Path $repoRoot "COPYRIGHT") -Destination (Join-Path $distRoot "COPYRIGHT")
$notices = (Get-Content -Raw -LiteralPath (Join-Path $repoRoot "THIRD_PARTY_NOTICES.md")).Replace("packages/ui/public/licenses/", "licenses/")
Set-Content -LiteralPath (Join-Path $distRoot "THIRD_PARTY_NOTICES.md") -Value $notices -Encoding utf8NoBOM
Copy-Item -LiteralPath (Join-Path $repoRoot "packages/ui/public/licenses") -Destination (Join-Path $distRoot "licenses") -Recurse
Invoke-Checked "third-party license assembly" "node.exe" @(
    (Join-Path $repoRoot "packages/desktop/scripts/write-third-party-licenses.mjs"),
    (Join-Path $distRoot "THIRD_PARTY_LICENSES"),
    "centaeris-runtime",
    "centaeris-tui",
    "--rust-only"
) $repoRoot

if ($systemSkillsBundle) {
    $packagedSystemSkills = Join-Path $distRoot "system-skills"
    Copy-Item -LiteralPath $resolvedSystemSkillsSource -Destination $packagedSystemSkills -Recurse
    $packagedBundle = Get-SystemSkillsBundle $packagedSystemSkills
    Assert-SystemSkillLicenses $packagedSystemSkills $packagedBundle
    if ($packagedBundle.digest -ne $systemSkillsBundle.digest) {
        throw "Packaged System Skills bundle digest mismatch"
    }
}

$version = Get-Version
$manifestFiles = @()
foreach ($file in Get-ChildItem -LiteralPath $distRoot -File -Recurse | Sort-Object FullName) {
    $manifestFiles += [ordered]@{
        path = [System.IO.Path]::GetRelativePath($distRoot, $file.FullName).Replace("\", "/")
        sizeBytes = $file.Length
        sha256 = (Get-FileSha256 $file.FullName)
    }
}
$manifest = [ordered]@{
    schema = "centaeris-package.v1"
    version = $version
    files = $manifestFiles
}
$manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $distRoot "centaeris-package.json") -Encoding utf8NoBOM

foreach ($requiredPath in @(
    "LICENSE.centaeris.txt",
    "COPYRIGHT",
    "THIRD_PARTY_NOTICES.md",
    "licenses/OFL-GoogleSansCode.txt",
    "licenses/OFL-NotoSansCJK.txt",
    "THIRD_PARTY_LICENSES/index.json"
)) {
    if ($manifest.files.path -notcontains $requiredPath) {
        throw "TUI package manifest is missing required license content: $requiredPath"
    }
}
if ($systemSkillsBundle) {
    foreach ($skillName in $systemSkillsBundle.skillNames) {
        $skillManifestPath = "system-skills/$skillName/SKILL.md"
        if ($manifest.files.path -notcontains $skillManifestPath) {
            throw "TUI package manifest is missing a bundled System Skill manifest"
        }
    }
}

$zipName = "centaeris-windows-x64.zip"
$zipPath = Join-Path (Join-Path $tuiDir "dist") $zipName
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}
Compress-Archive -Path (Join-Path $distRoot "*") -DestinationPath $zipPath -CompressionLevel Optimal

Write-Host "[tui-build] complete" -ForegroundColor Green
Write-Host "Package: $distRoot"
Write-Host "Archive: $zipPath"
Write-Host "TUI size: $((Get-Item -LiteralPath (Join-Path $distRoot $tuiName)).Length) bytes"
Write-Host "Runtime size: $((Get-Item -LiteralPath (Join-Path $distRoot $runtimeName)).Length) bytes"
Write-Host "Archive size: $((Get-Item -LiteralPath $zipPath).Length) bytes"
Write-Host "Version: $version"
if ($systemSkillsBundle) {
    Write-Host "System Skills: $($systemSkillsBundle.skillNames.Count) ($($systemSkillsBundle.digest))"
}
