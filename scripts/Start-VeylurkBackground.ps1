#requires -Version 5.1
[CmdletBinding()]
param([ValidateRange(1, 65535)][int]$Port = 8794)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
$supervisorLock = $null
$credentials = $null
$process = $null
$info = $null
$code = 1
try {
    $package = Get-VeylurkPackagePath $PSScriptRoot
    $data = Join-Path $package 'data'
    Assert-VeylurkOrdinaryPath $data
    [void][IO.Directory]::CreateDirectory($data)
    Assert-VeylurkOrdinaryPath (Join-Path $data 'supervisor.lock')
    $supervisorLock = [IO.File]::Open((Join-Path $data 'supervisor.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    Assert-VeylurkReady $package $Port
    $credentials = Read-VeylurkCredentials $package
    $info = New-VeylurkProcessInfo (Join-Path $package 'veylurk.exe') $package @('--database', (Join-Path $data 'observations.sqlite3'), '--port', [string]$Port) $credentials
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    Write-VeylurkLifecycle $package 'starting'
    if (-not $process.Start()) { throw 'Background application start failed.' }
    $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_ID')
    $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_SECRET')
    $credentials = $null
    # Drain and discard application output: never persist secrets or unbounded output.
    $process.BeginOutputReadLine()
    $process.BeginErrorReadLine()
    $process.WaitForExit()
    $code = $process.ExitCode
    $event = if ($code -eq 0) { 'stopped' } else { 'failed' }
    Write-VeylurkLifecycle $package $event $code
} catch {
    # Avoid logging exception text, arguments, credentials, URLs, or raw app output.
    [Console]::Error.WriteLine('Veylurk background supervision failed. Check the package, port and saved credentials; no process was killed.')
} finally {
    if ($null -ne $info) {
        $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_ID')
        $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_SECRET')
    }
    $credentials = $null
    if ($null -ne $process) { $process.Dispose() }
    if ($null -ne $supervisorLock) { $supervisorLock.Dispose() }
}
exit $code
