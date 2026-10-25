#requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
$package = Get-VeylurkPackagePath $PSScriptRoot
$task = Get-VeylurkOwnedTask $package
if ($null -ne $task) {
    # Unregistering removes future starts; it does not call Stop-ScheduledTask.
    Unregister-ScheduledTask -TaskName $task.TaskName -Confirm:$false
    Write-Output 'Removed login startup. A running application remains open; use Stop-Veylurk.ps1 to stop it gracefully.'
} else { Write-Output 'No login startup task exists for this package and user.' }
