#requires -Version 5.1
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Destination, [string]$PackageDirectory = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
New-VeylurkOfflineBackup (Get-VeylurkPackagePath $PackageDirectory) $Destination
Write-Output 'Offline backup created and verified. Credentials remain encrypted.'
