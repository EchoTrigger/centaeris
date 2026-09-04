param(
    [string]$Release = "latest",
    [string]$InstallRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoName = "EchoTrigger/Centaeris"
$tuiHome = if (-not [string]::IsNullOrWhiteSpace($InstallRoot)) {
    [System.IO.Path]::GetFullPath($InstallRoot)
} elseif ($env:CENTAERIS_TUI_HOME) {
    [System.IO.Path]::GetFullPath($env:CENTAERIS_TUI_HOME)
} else {
    [System.IO.Path]::GetFullPath((Join-Path $env:USERPROFILE ".centaeris/tui"))
}
$standaloneRoot = Join-Path $tuiHome "packages/standalone"
$releasesDir = Join-Path $standaloneRoot "releases"
$currentLink = Join-Path $standaloneRoot "current"
$binDir = Join-Path $standaloneRoot "bin"
$binPath = Join-Path $binDir "centa.exe"

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Resolve-Version {
    param([string]$Requested)
    if ($Requested -eq "latest" -or [string]::IsNullOrWhiteSpace($Requested)) {
        $meta = Invoke-RestMethod -Uri "https://api.github.com/repos/$repoName/releases/latest" -Headers @{ "User-Agent" = "centaeris-installer" }
        return $meta.tag_name
    }
    return $Requested
}

function Get-Release {
    param(
        [Parameter(Mandatory = $true)][string]$Version
    )
    return Invoke-RestMethod -Uri "https://api.github.com/repos/$repoName/releases/tags/$Version" -Headers @{ "User-Agent" = "centaeris-installer" }
}

function Get-Asset {
    param(
        [Parameter(Mandatory = $true)][object]$Release,
        [Parameter(Mandatory = $true)][string]$AssetName
    )
    $asset = $Release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
    if ($null -eq $asset) {
        throw "release asset not found: $AssetName"
    }
    return $asset
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

$version = Resolve-Version -Requested $Release
$release = Get-Release -Version $version
$assetName = "centaeris-windows-x64.zip"
$tempZip = Join-Path $env:TEMP "centaeris-$version.zip"

Write-Step "Centaeris TUI installer: version $version"

$asset = Get-Asset -Release $release -AssetName $assetName
$expectedDigest = $asset.digest
if (-not $expectedDigest) {
    throw "release asset has no digest: $assetName"
}
if ($expectedDigest -notmatch "^sha256:([0-9a-fA-F]{64})$") {
    throw "release asset digest has unexpected format: $expectedDigest"
}
$expectedHash = $Matches[1].ToLowerInvariant()

Write-Step "Downloading $assetName"
$ProgressPreference = "SilentlyContinue"
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $tempZip -UseBasicParsing
if (-not (Test-Path -LiteralPath $tempZip)) {
    throw "download failed: $assetName"
}
$actualHash = Get-Sha256 $tempZip
if ($actualHash -ne $expectedHash) {
    throw "digest mismatch: expected $expectedHash, got $actualHash"
}
Write-Step "Digest verified"

$installDir = Join-Path $releasesDir $version
New-Item -ItemType Directory -Path $installDir -Force | Out-Null

Write-Step "Extracting to $installDir"
Expand-Archive -LiteralPath $tempZip -DestinationPath $installDir -Force

$manifestPath = Join-Path $installDir "centaeris-package.json"
if (-not (Test-Path -LiteralPath $manifestPath)) {
    throw "package manifest missing: $manifestPath"
}
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if ($manifest.schema -ne "centaeris-package.v1") {
    throw "unsupported package manifest schema: $($manifest.schema)"
}
foreach ($file in $manifest.files) {
    $filePath = Join-Path $installDir $file.path
    if (-not (Test-Path -LiteralPath $filePath)) {
        throw "package manifest file missing: $($file.path)"
    }
    $fileSize = (Get-Item -LiteralPath $filePath).Length
    if ($fileSize -ne $file.sizeBytes) {
        throw "package file size mismatch: $($file.path)"
    }
    $fileHash = Get-Sha256 $filePath
    if ($fileHash -ne $file.sha256.ToLowerInvariant()) {
        throw "package file digest mismatch: $($file.path)"
    }
}
Write-Step "Package manifest verified"

if (Test-Path -LiteralPath $currentLink) {
    $item = Get-Item -LiteralPath $currentLink -Force
    if ($item.LinkType) {
        Remove-Item -LiteralPath $currentLink -Force
    } else {
        Remove-Item -LiteralPath $currentLink -Recurse -Force
    }
}
New-Item -ItemType Junction -Path $currentLink -Target $installDir | Out-Null

New-Item -ItemType Directory -Path $binDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $currentLink "centa.exe") -Destination $binPath -Force
Write-Step "Installed: $binPath"
Write-Step "Current version: $version"
Write-Step "Run: & '$binPath'"
