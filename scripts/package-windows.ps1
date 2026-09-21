param(
    [Parameter(Mandatory = $true)] [string]$BrowserDirectory,
    [Parameter(Mandatory = $true)] [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$browserRoot = (Resolve-Path -LiteralPath $BrowserDirectory).Path
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
$helperRoot = Join-Path $repoRoot 'browser-helper'
$nodePath = (Get-Command node -ErrorAction Stop).Source
$nodeDirectory = Split-Path -Parent $nodePath
$nodeLicense = Join-Path $nodeDirectory 'LICENSE'
$playwrightLicense = Join-Path $helperRoot 'node_modules\playwright\LICENSE'
$coreLicense = Join-Path $helperRoot 'node_modules\playwright-core\LICENSE'
$requiredFiles = @(
    (Join-Path $repoRoot 'target\release\veylurk-browser-probe.exe'),
    $nodePath,
    $nodeLicense,
    (Join-Path $repoRoot 'LICENSE'),
    (Join-Path $helperRoot 'helper.mjs'),
    (Join-Path $helperRoot 'native-response.mjs'),
    (Join-Path $helperRoot 'test-helper.mjs'),
    (Join-Path $helperRoot 'test-native-response.mjs'),
    (Join-Path $helperRoot 'package.json'),
    (Join-Path $helperRoot 'package-lock.json'),
    $playwrightLicense,
    $coreLicense
)
foreach ($required in $requiredFiles) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required bundle input is missing: $required"
    }
}
if (@(Get-ChildItem -LiteralPath $browserRoot -Directory -Filter 'chromium-*').Count -lt 1) {
    throw "No Playwright Chromium installation found in $browserRoot"
}
$browserRegistry = Join-Path $helperRoot 'node_modules\playwright-core\browsers.json'
if (-not (Test-Path -LiteralPath $browserRegistry -PathType Leaf)) {
    throw "Playwright browser registry is missing: $browserRegistry"
}
$chromiumVersion = ((Get-Content -Raw -LiteralPath $browserRegistry | ConvertFrom-Json).browsers |
    Where-Object { $_.name -eq 'chromium' } | Select-Object -First 1).browserVersion
if ($chromiumVersion -notmatch '^\d+\.\d+\.\d+\.\d+$') {
    throw 'Could not determine the matched Chromium source version'
}
$chromiumLicenseUrl = "https://raw.githubusercontent.com/chromium/chromium/$chromiumVersion/LICENSE"
if (Test-Path -LiteralPath $outputRoot) {
    throw "Refusing to overwrite existing bundle directory: $outputRoot"
}

New-Item -ItemType Directory -Path $outputRoot | Out-Null
$outHelper = Join-Path $outputRoot 'browser-helper'
$outNotices = Join-Path $outputRoot 'third-party-notices'
New-Item -ItemType Directory -Path $outHelper, $outNotices | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot 'target\release\veylurk-browser-probe.exe') -Destination $outputRoot
Copy-Item -LiteralPath $nodePath -Destination $outputRoot
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination $outputRoot
Copy-Item -LiteralPath (Join-Path $helperRoot 'helper.mjs') -Destination $outHelper
Copy-Item -LiteralPath (Join-Path $helperRoot 'native-response.mjs') -Destination $outHelper
Copy-Item -LiteralPath (Join-Path $helperRoot 'test-helper.mjs') -Destination $outHelper
Copy-Item -LiteralPath (Join-Path $helperRoot 'test-native-response.mjs') -Destination $outHelper
Copy-Item -LiteralPath (Join-Path $helperRoot 'fixtures') -Destination $outHelper -Recurse
Copy-Item -LiteralPath (Join-Path $helperRoot 'package.json') -Destination $outHelper
Copy-Item -LiteralPath (Join-Path $helperRoot 'package-lock.json') -Destination $outHelper
Copy-Item -LiteralPath (Join-Path $helperRoot 'node_modules') -Destination $outHelper -Recurse -Force
Copy-Item -LiteralPath $browserRoot -Destination (Join-Path $outputRoot 'browsers') -Recurse -Force
Copy-Item -LiteralPath $nodeLicense -Destination (Join-Path $outNotices 'NODE-LICENSE.txt')
Copy-Item -LiteralPath $playwrightLicense -Destination (Join-Path $outNotices 'PLAYWRIGHT-LICENSE.txt')
Copy-Item -LiteralPath $coreLicense -Destination (Join-Path $outNotices 'PLAYWRIGHT-CORE-LICENSE.txt')
Invoke-WebRequest -Uri $chromiumLicenseUrl -OutFile (Join-Path $outNotices 'CHROMIUM-LICENSE.txt') -TimeoutSec 30
if (-not (Test-Path -LiteralPath (Join-Path $outNotices 'CHROMIUM-LICENSE.txt') -PathType Leaf)) {
    throw 'Matched Chromium source license could not be downloaded'
}
$chromiumCredits = Get-ChildItem -LiteralPath $browserRoot -Recurse -File -Filter 'credits.html' |
    Select-Object -First 1
if ($null -ne $chromiumCredits) {
    Copy-Item -LiteralPath $chromiumCredits.FullName -Destination (Join-Path $outNotices 'CHROMIUM-CREDITS.html')
}

$chromiumCount = @(Get-ChildItem -LiteralPath (Join-Path $outputRoot 'browsers') -Directory -Filter 'chromium-*').Count
if ($chromiumCount -lt 1) { throw 'Packaged Chromium directory is missing' }
$notice = @'
# Third-party components

This Stage 3 probe bundle contains Node.js, Playwright, Playwright Core, and the Chromium build matched to the pinned Playwright version. Their license files are included in this directory. The Chromium license is from the matching source version; the bundled browser may contain additional third-party components and should be inspected via Chromium's own credits. The Veylurk probe source is released under the repository LICENSE.

The bundle is an experimental probe, not the Veylurk service. It uses a fresh browser profile and does not include or require a personal browser profile, credentials, or Twitch tokens.
'@
Set-Content -LiteralPath (Join-Path $outNotices 'README.md') -Value "$notice`nMatched Chromium source version: $chromiumVersion`nChromium license source: $chromiumLicenseUrl`n" -Encoding utf8
Write-Output $outputRoot
