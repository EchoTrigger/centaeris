param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$BaseSha,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$HeadSha
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

git cat-file -e "$BaseSha^{commit}"
git cat-file -e "$HeadSha^{commit}"

$commits = @(git rev-list --reverse "$BaseSha..$HeadSha")
if ($commits.Count -eq 0) {
    Write-Host "No contribution commits to check."
    exit 0
}

$invalid = @()
foreach ($commit in $commits) {
    $message = git show --no-show-signature --quiet --format=%B $commit
    $trailers = @($message | git interpret-trailers --parse)
    $signOffs = @($trailers | Where-Object {
        $_ -match "(?i)^Signed-off-by:\s+[^<>\r\n]+\s+<[^<>\s@]+@[^<>\s@]+>$"
    })
    if ($signOffs.Count -eq 0) {
        $subject = git show --no-show-signature --quiet --format=%s $commit
        $invalid += [pscustomobject]@{
            Commit = $commit.Substring(0, 12)
            Subject = $subject
        }
    }
}

if ($invalid.Count -gt 0) {
    Write-Host "Every contribution commit must contain a valid 'Signed-off-by: Name <email>' trailer. Recreate the commit with 'git commit --signoff'." -ForegroundColor Red
    $invalid | Format-Table -AutoSize | Out-String | Write-Host
    exit 1
}

Write-Host "DCO sign-off verified for $($commits.Count) contribution commit(s)."
