#requires -Version 5.1
[CmdletBinding()]
param([ValidateRange(1, 65535)][int]$Port = 8794, [ValidateRange(1, 300)][int]$TimeoutSeconds = 60)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
$package = Get-VeylurkPackagePath $PSScriptRoot
Stop-VeylurkGracefully $package $Port $TimeoutSeconds
Write-Output 'Veylurk stopped gracefully. Login startup remains configured if installed.'
