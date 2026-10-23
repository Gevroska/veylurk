#requires -Version 5.1
# Offline tests use invented values and Windows PowerShell as the child, never Veylurk.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Launcher.ps1')
function Assert-Fixture { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }
foreach ($file in @('Veylurk-Launcher.ps1', 'Start-Veylurk.ps1', 'Setup-Twitch.ps1', 'Test-VeylurkLauncher.ps1')) {
    $tokens = $null; $parseErrors = $null
    $null = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $file), [ref]$tokens, [ref]$parseErrors)
    Assert-Fixture ($parseErrors.Count -eq 0) 'Launcher parsing failed.'
}
$directory = Join-Path ([IO.Path]::GetTempPath()) ('veylurk launcher fixture ' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($directory)
$childScript = Join-Path $directory 'child fixture.ps1'
@'
param([string]$Space, [string]$Quote, [string]$Trailing)
if ($env:VEYLURK_TWITCH_CLIENT_ID -cne 'dummy-client') { exit 21 }
if ($env:VEYLURK_TWITCH_CLIENT_SECRET -cne 'dummy-secret') { exit 22 }
if ($Space -cne 'path with spaces' -or $Quote -cne 'embedded"quote' -or $Trailing -cne 'trailing slash\') { exit 23 }
exit 0
'@ | Set-Content -LiteralPath $childScript -Encoding UTF8
$originalId = [Environment]::GetEnvironmentVariable('VEYLURK_TWITCH_CLIENT_ID', 'Process')
$originalSecret = [Environment]::GetEnvironmentVariable('VEYLURK_TWITCH_CLIENT_SECRET', 'Process')
$credentials = [pscustomobject]@{ ClientId = 'dummy-client'; ClientSecret = 'dummy-secret' }
$windowsPowerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$info = New-VeylurkProcessInfo $windowsPowerShell $directory @('-NoProfile', '-NonInteractive', '-File', $childScript, 'path with spaces', 'embedded"quote', 'trailing slash\') $credentials
$info.CreateNoWindow = $true
$info.RedirectStandardOutput = $true
$info.RedirectStandardError = $true
Assert-Fixture (-not $info.Arguments.Contains('dummy-secret')) 'Secret appeared in child arguments.'
$child = New-Object Diagnostics.Process
$child.StartInfo = $info
try {
    [void]$child.Start()
    $output = $child.StandardOutput.ReadToEnd()
    $errors = $child.StandardError.ReadToEnd()
    $child.WaitForExit()
    Assert-Fixture ($child.ExitCode -eq 0) 'Dummy child environment or argument quoting failed.'
    Assert-Fixture ([string]::IsNullOrEmpty($output) -and [string]::IsNullOrEmpty($errors)) 'Dummy child emitted unexpected output.'
} finally { $child.Dispose() }
Assert-Fixture ([Environment]::GetEnvironmentVariable('VEYLURK_TWITCH_CLIENT_ID', 'Process') -ceq $originalId) 'Parent client ID environment changed.'
Assert-Fixture ([Environment]::GetEnvironmentVariable('VEYLURK_TWITCH_CLIENT_SECRET', 'Process') -ceq $originalSecret) 'Parent secret environment changed.'
$withoutCredentials = New-VeylurkProcessInfo $windowsPowerShell $directory @('-NoProfile') $null
Assert-Fixture (-not $withoutCredentials.EnvironmentVariables.ContainsKey('VEYLURK_TWITCH_CLIENT_ID')) 'No-credential child retained a client ID.'
Assert-Fixture (-not $withoutCredentials.EnvironmentVariables.ContainsKey('VEYLURK_TWITCH_CLIENT_SECRET')) 'No-credential child retained a secret.'
Save-VeylurkCredentials $directory $credentials
$path = Get-VeylurkCredentialPath $directory
$ciphertext = [IO.File]::ReadAllBytes($path)
Assert-Fixture (-not [Text.Encoding]::UTF8.GetString($ciphertext).Contains('dummy-secret')) 'Saved file contained plaintext.'
$plain = [Security.Cryptography.ProtectedData]::Unprotect($ciphertext, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
try {
    $roundtrip = [Text.Encoding]::UTF8.GetString($plain) | ConvertFrom-Json
    Assert-Fixture ($roundtrip.ClientId -ceq 'dummy-client' -and $roundtrip.ClientSecret -ceq 'dummy-secret') 'DPAPI round trip failed.'
} finally { [Array]::Clear($plain, 0, $plain.Length) }
Save-VeylurkCredentials $directory ([pscustomobject]@{ ClientId = 'dummy-client'; ClientSecret = 'dummy-replacement' })
$plain = [Security.Cryptography.ProtectedData]::Unprotect([IO.File]::ReadAllBytes($path), $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
try {
    $roundtrip = [Text.Encoding]::UTF8.GetString($plain) | ConvertFrom-Json
    Assert-Fixture ($roundtrip.ClientSecret -ceq 'dummy-replacement') 'Atomic encrypted replacement failed.'
} finally { [Array]::Clear($plain, 0, $plain.Length) }
# Dummy fixture files are retained under the system temporary directory; no recursive cleanup.
[Console]::WriteLine('Launcher offline checks passed: Windows PowerShell parser, child-only credentials, quoted arguments, silent output, DPAPI round trip and replacement.')
