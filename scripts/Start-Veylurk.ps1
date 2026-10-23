#requires -Version 5.1
[CmdletBinding()]
param([ValidateRange(1, 65535)][int]$Port = 8794)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Launcher.ps1')
$credentials = $null
try {
    Assert-VeylurkReady $PSScriptRoot $Port
    $credentials = Read-VeylurkCredentials $PSScriptRoot
    if ($null -eq $credentials) { [Console]::WriteLine('Starting chat collection without optional broadcast credentials. Use Setup-Twitch.ps1 to configure them.') }
    $code = Start-VeylurkApplication $PSScriptRoot $Port $credentials
    exit $code
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
} finally { $credentials = $null }
