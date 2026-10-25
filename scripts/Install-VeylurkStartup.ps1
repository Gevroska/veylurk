#requires -Version 5.1
[CmdletBinding()]
param([ValidateRange(1, 65535)][int]$Port = 8794)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
$package = Get-VeylurkPackagePath $PSScriptRoot
$name = Register-VeylurkStartup $package $Port
Write-Output "Installed login startup task $name for the current user. It has not been started."
