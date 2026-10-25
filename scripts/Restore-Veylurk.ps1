#requires -Version 5.1
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BackupDirectory, [Parameter(Mandatory = $true)][string]$DataDirectory)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
Restore-VeylurkOfflineBackup ([IO.Path]::GetFullPath($BackupDirectory)) $DataDirectory
Write-Output 'Backup restored into a new data directory. Existing history was preserved.'
