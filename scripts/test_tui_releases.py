"""Mock release API responses; never download or install a product."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
WORKSPACE = {"tag_name": "workspace-v9.0.0", "draft": False, "prerelease": False, "assets": []}
TUI = {"tag_name": "tui-v0.1.0", "draft": False, "prerelease": False,
       "assets": [{"name": "centaeris-windows-x64.zip"}]}


class TuiReleaseTests(unittest.TestCase):
    def powershell(self, pages, expect):
        script = r'''
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PWD 'scripts/install-tui.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Installer syntax error' }
foreach ($function in $ast.FindAll({param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst]}, $false)) {
    . ([scriptblock]::Create($function.Extent.Text))
}
$repoName = 'synthetic/repository'; $assetName = 'centaeris-windows-x64.zip'
$script:pages = ConvertFrom-Json -NoEnumerate $env:TEST_RELEASE_PAGES
function Invoke-RestMethod { param($Uri, $Headers)
    if ($Uri -match '/latest$') { return $script:pages[0][0] }
    if ($Uri -notmatch '[?&]page=([0-9]+)') { throw 'Expected a paginated product lookup' }
    Write-Output -NoEnumerate $script:pages[[int]$Matches[1] - 1]
}
try { $actual = Resolve-Version -Requested 'latest' } catch {
    if ($env:TEST_EXPECTED_TAG -eq 'FAIL') { exit 0 }; throw
}
if ($actual -ne $env:TEST_EXPECTED_TAG) { throw "Selected wrong product: $actual" }
'''
        result = subprocess.run(["pwsh", "-NoProfile", "-NonInteractive", "-Command", script], cwd=ROOT,
                                env={**os.environ, "TEST_RELEASE_PAGES": json.dumps(pages), "TEST_EXPECTED_TAG": expect},
                                text=True, encoding="utf-8", capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_windows_skips_newer_workspace_and_prerelease(self):
        self.powershell([[WORKSPACE, {**TUI, "prerelease": True}, TUI]], TUI["tag_name"])

    def test_windows_searches_later_pages(self):
        self.powershell([[WORKSPACE] * 100, [TUI]], TUI["tag_name"])

    def test_windows_missing_tui_asset_fails_closed(self):
        self.powershell([[WORKSPACE]], "FAIL")

    def test_shell_skips_other_products_without_new_json_dependencies(self):
        source = (ROOT / "scripts/install-tui.sh").read_text(encoding="utf-8")
        header = source.split('\nif [ "$(uname -s)"', 1)[0]
        response = json.dumps([WORKSPACE, TUI])
        command = header + "\nfetch() { printf '%s\\n' " + shlex.quote(response) + "; }\n"
        command += "ASSET_NAME=centaeris-windows-x64.zip\nresolve_version latest\n"
        bash = str(Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe") if os.name == "nt" else "bash"
        result = subprocess.run([bash, "-s"], input=command, cwd=ROOT, text=True, encoding="utf-8", capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), TUI["tag_name"])


if __name__ == "__main__":
    unittest.main()
