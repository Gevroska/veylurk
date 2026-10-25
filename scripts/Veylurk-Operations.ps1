# Shared Windows PowerShell 5.1 operations. Dot-sourcing does not start the app or change tasks.
Set-StrictMode -Version 2.0
. (Join-Path $PSScriptRoot 'Veylurk-Launcher.ps1')

function Get-VeylurkPackagePath {
    param([string]$PackageDirectory)
    $path = [IO.Path]::GetFullPath($PackageDirectory)
    if ($path -ne [IO.Path]::GetPathRoot($path)) { $path = $path.TrimEnd('\') }
    if (-not [IO.Directory]::Exists($path)) { throw 'The selected package directory does not exist.' }
    Assert-VeylurkOrdinaryPath $path
    return $path
}

function Assert-VeylurkOrdinaryPath {
    param([string]$Path)
    $cursor = [IO.Path]::GetFullPath($Path)
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            if ((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw 'Reparse points are not supported for operation or backup paths.'
            }
        }
        $parent = [IO.Path]::GetDirectoryName($cursor)
        if ($parent -eq $cursor) { break }
        $cursor = $parent
    }
}

function Get-VeylurkTaskName {
    param([string]$PackageDirectory)
    $hash = [Security.Cryptography.SHA256]::Create()
    try {
        $owner = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $bytes = $hash.ComputeHash([Text.Encoding]::UTF8.GetBytes(([IO.Path]::GetFullPath($PackageDirectory).TrimEnd('\').ToLowerInvariant() + '|' + $owner)))
        return 'Veylurk-' + ([BitConverter]::ToString($bytes).Replace('-', '').Substring(0, 16))
    } finally { $hash.Dispose() }
}

function Register-VeylurkStartup {
    param([string]$PackageDirectory, [int]$Port)
    if (-not [IO.File]::Exists((Join-Path $PackageDirectory 'veylurk.exe'))) { throw 'Extract the application and all scripts together into a stable folder first.' }
    $null = Get-VeylurkOwnedTask $PackageDirectory
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $action = New-ScheduledTaskAction -Execute $powershell -Argument (Get-VeylurkTaskArguments $PackageDirectory $Port) -WorkingDirectory $PackageDirectory
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $identity
    $principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -Hidden
    $name = Get-VeylurkTaskName $PackageDirectory
    $null = Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'Veylurk per-user login startup. Stop gracefully using Stop-Veylurk.ps1.' -Force
    return $name
}

function Get-VeylurkTaskArguments {
    param([string]$PackageDirectory, [int]$Port)
    return '-NoProfile -NonInteractive -WindowStyle Hidden -File ' + (ConvertTo-VeylurkArgument (Join-Path $PackageDirectory 'Start-VeylurkBackground.ps1')) + ' -Port ' + $Port
}

function Get-VeylurkOwnedTask {
    param([string]$PackageDirectory)
    $task = Get-ScheduledTask -TaskName (Get-VeylurkTaskName $PackageDirectory) -ErrorAction SilentlyContinue
    if ($null -eq $task) { return $null }
    $expected = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $prefix = '-NoProfile -NonInteractive -WindowStyle Hidden -File ' + (ConvertTo-VeylurkArgument (Join-Path $PackageDirectory 'Start-VeylurkBackground.ps1')) + ' -Port '
    $pattern = '^' + [regex]::Escape($prefix) + '([1-9][0-9]{0,4})$'
    if (@($task.Actions).Count -ne 1 -or $task.Actions[0].Execute -ine $expected -or
        $task.Actions[0].WorkingDirectory -ine $PackageDirectory -or $task.Actions[0].Arguments -notmatch $pattern) {
        throw 'A task with this package name has different actions. It was left unchanged.'
    }
    if ([int]$Matches[1] -gt 65535) { throw 'The existing task has an invalid port and was left unchanged.' }
    return $task
}

function Write-VeylurkLifecycle {
    param([string]$PackageDirectory, [ValidateSet('starting', 'stopped', 'failed')][string]$Event, [int]$Code = 0)
    $log = Join-Path $PackageDirectory 'data\background.log'
    Assert-VeylurkOrdinaryPath $log
    Assert-VeylurkOrdinaryPath ($log + '.previous')
    if ((Test-Path -LiteralPath $log) -and (Get-Item -LiteralPath $log).Length -gt 262144) {
        $previous = $log + '.previous'
        if (Test-Path -LiteralPath $previous) { [IO.File]::Delete($previous) }
        [IO.File]::Move($log, $previous)
    }
    [IO.File]::AppendAllText($log, ([DateTime]::UtcNow.ToString('o') + ' ' + $Event + ' exit=' + $Code + [Environment]::NewLine))
}

function Get-VeylurkRuntime {
    param([int]$Port)
    Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/runtime" -TimeoutSec 5
}

function Assert-VeylurkRuntime {
    param([string]$PackageDirectory, [int]$Port, $Runtime)
    $expectedExe = Join-Path $PackageDirectory 'veylurk.exe'
    $expectedDb = Join-Path $PackageDirectory 'data\observations.sqlite3'
    if ($Runtime.port -ne $Port -or [string]::IsNullOrEmpty($Runtime.instance_id) -or
        (ConvertTo-VeylurkNormalPath $Runtime.executable) -ine $expectedExe -or
        (ConvertTo-VeylurkNormalPath $Runtime.database) -ine $expectedDb) { throw 'The listener does not identify this package and database.' }
    $app = Get-Process -Id $Runtime.pid -ErrorAction Stop
    if ($app.Path -ine $expectedExe) { throw 'The running executable does not match this package.' }
    $listeners = @(Get-NetTCPConnection -LocalAddress '127.0.0.1' -LocalPort $Port -State Listen -ErrorAction Stop)
    if ($listeners.Count -ne 1 -or $listeners[0].OwningProcess -ne $app.Id) { throw 'The local listener does not belong to the identified process.' }
    return $app
}

function ConvertTo-VeylurkNormalPath {
    param([string]$Path)
    if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) { $Path = '\\' + $Path.Substring(8) }
    elseif ($Path.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) { $Path = $Path.Substring(4) }
    return [IO.Path]::GetFullPath($Path)
}

function Stop-VeylurkGracefully {
    param([string]$PackageDirectory, [int]$Port, [int]$TimeoutSeconds = 60)
    $runtime = Get-VeylurkRuntime $Port
    $app = Assert-VeylurkRuntime $PackageDirectory $Port $runtime
    try {
        # Open and retain the process handle before requesting exit. Otherwise a
        # short-lived process can disappear before .NET can obtain its exit code.
        $null = $app.Handle
        # Check again immediately before the mutation; never stop a replacement instance.
        $again = Get-VeylurkRuntime $Port
        if ($again.instance_id -cne $runtime.instance_id) { throw 'The application instance changed. No shutdown was requested.' }
        $body = @{ instance_id = $runtime.instance_id } | ConvertTo-Json -Compress
        $result = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$Port/api/shutdown" -Headers @{ Origin = "http://127.0.0.1:$Port" } -ContentType 'application/json' -Body $body -TimeoutSec 5
        if ($result.instance_id -cne $runtime.instance_id -or $result.state -cne 'stopping') { throw 'The application did not acknowledge this shutdown.' }
        if (-not $app.WaitForExit($TimeoutSeconds * 1000)) { throw 'Graceful shutdown is still pending. No process was killed; wait and retry.' }
        $exitCode = $app.ExitCode
        if ($null -eq $exitCode -or $exitCode -ne 0) {
            throw 'Veylurk exited without confirming a clean shutdown. History and WAL were retained; inspect them before attempting an offline backup.'
        }
    } finally { $app.Dispose() }
}

function Open-VeylurkOfflineLock {
    param([string]$Database)
    Assert-VeylurkOrdinaryPath $Database
    Assert-VeylurkOrdinaryPath ($Database + '.lock')
    $handle = $null
    try {
        # Windows Rust std::fs::File::try_lock locks the whole file. FileShare.None
        # also refuses its existing read/write handle, and holds off future opens.
        $handle = [IO.File]::Open(($Database + '.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $handle.Lock(0, 1)
        return $handle
    } catch {
        if ($null -ne $handle) { $handle.Dispose() }
        throw 'The database is in use or its exclusive lock is unavailable. Stop Veylurk and retry.'
    }
}

function Test-VeylurkBackup {
    param([string]$BackupDirectory)
    Assert-VeylurkOrdinaryPath $BackupDirectory
    $manifestPath = Join-Path $BackupDirectory 'manifest.json'
    Assert-VeylurkOrdinaryPath $manifestPath
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $allowed = @('observations.sqlite3', 'twitch-credentials.dpapi')
    if ($manifest.version -ne 1 -or @($manifest.files).Count -lt 1 -or @($manifest.files).Count -gt 2) { throw 'Unsupported backup manifest.' }
    $seen = @{}
    foreach ($entry in $manifest.files) {
        if ($allowed -cnotcontains $entry.name -or $seen.ContainsKey($entry.name) -or $entry.sha256 -notmatch '^[a-fA-F0-9]{64}$') { throw 'Invalid backup file manifest.' }
        $seen[$entry.name] = $true
        $path = Join-Path $BackupDirectory $entry.name
        Assert-VeylurkOrdinaryPath $path
        $file = Get-Item -LiteralPath $path -ErrorAction Stop
        if ($file.PSIsContainer -or $file.Length -ne $entry.bytes -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256) { throw 'Backup integrity check failed.' }
    }
    if (-not $seen.ContainsKey('observations.sqlite3')) { throw 'Backup has no history database.' }
    foreach ($file in Get-ChildItem -LiteralPath $BackupDirectory -Force) {
        if ($file.Name -cne 'manifest.json' -and -not $seen.ContainsKey($file.Name)) { throw 'Backup contains an unlisted file.' }
    }
    return $manifest
}

function New-VeylurkOfflineBackup {
    param([string]$PackageDirectory, [string]$Destination)
    $source = Join-Path $PackageDirectory 'data'
    $database = Join-Path $source 'observations.sqlite3'
    if (-not [IO.File]::Exists($database)) { throw 'The package has no history database to back up.' }
    $destinationPath = [IO.Path]::GetFullPath($Destination)
    Assert-VeylurkOrdinaryPath $destinationPath
    if (Test-Path -LiteralPath $destinationPath) { throw 'Choose a new backup directory; existing backups are never overwritten.' }
    $parent = Split-Path -Parent $destinationPath
    if (-not [IO.Directory]::Exists($parent)) { throw 'The backup parent directory must already exist.' }
    $lock = Open-VeylurkOfflineLock $database
    $stage = Join-Path $parent ('.veylurk-backup-' + [guid]::NewGuid().ToString('N'))
    try {
        $wal = $database + '-wal'
        Assert-VeylurkOrdinaryPath $wal
        if ([IO.File]::Exists($wal) -and (Get-Item -LiteralPath $wal).Length -gt 0) { throw 'The stopped database still has a nonempty WAL. Reopen and gracefully stop Veylurk before backing up.' }
        [void][IO.Directory]::CreateDirectory($stage)
        $files = @()
        foreach ($name in @('observations.sqlite3', 'twitch-credentials.dpapi')) {
            $path = Join-Path $source $name
            Assert-VeylurkOrdinaryPath $path
            if ([IO.File]::Exists($path)) {
                $copy = Join-Path $stage $name
                [IO.File]::Copy($path, $copy, $false)
                $files += [pscustomobject]@{ name = $name; bytes = (Get-Item -LiteralPath $copy).Length; sha256 = (Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash.ToLowerInvariant() }
            }
        }
        [pscustomobject]@{ version = 1; created_at = [DateTime]::UtcNow.ToString('o'); credentials = 'Windows DPAPI CurrentUser ciphertext, unchanged'; files = $files } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $stage 'manifest.json') -Encoding UTF8
        $null = Test-VeylurkBackup $stage
        [IO.Directory]::Move($stage, $destinationPath)
    } finally { $lock.Dispose() }
}

function Restore-VeylurkOfflineBackup {
    param([string]$BackupDirectory, [string]$DataDirectory)
    $manifest = Test-VeylurkBackup $BackupDirectory
    $target = [IO.Path]::GetFullPath($DataDirectory)
    Assert-VeylurkOrdinaryPath $target
    $backupRoot = [IO.Path]::GetFullPath($BackupDirectory).TrimEnd('\') + '\'
    if ($target.StartsWith($backupRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Restore data must be outside the backup directory.' }
    if (Test-Path -LiteralPath $target) { throw 'Restore needs a new data directory. Existing history is never overwritten.' }
    $parent = Split-Path -Parent $target
    if (-not [IO.Directory]::Exists($parent)) { throw 'The restore parent directory must already exist.' }
    $stage = Join-Path $parent ('.veylurk-restore-' + [guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($stage)
    foreach ($entry in $manifest.files) { [IO.File]::Copy((Join-Path $BackupDirectory $entry.name), (Join-Path $stage $entry.name), $false) }
    [IO.File]::Copy((Join-Path $BackupDirectory 'manifest.json'), (Join-Path $stage 'manifest.json'), $false)
    $null = Test-VeylurkBackup $stage
    # Do not install the transport manifest as application data.
    [IO.File]::Delete((Join-Path $stage 'manifest.json'))
    [IO.Directory]::Move($stage, $target)
}
