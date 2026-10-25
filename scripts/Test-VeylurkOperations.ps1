#requires -Version 5.1
# Isolated filesystem/DPAPI fixtures. Task Scheduler and HTTP calls are mocked.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')
function Assert-Fixture { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }
function Assert-Rejected { param([scriptblock]$Action, [string]$Message) $failed = $false; try { & $Action } catch { $failed = $true }; Assert-Fixture $failed $Message }
foreach ($file in Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*.ps1') {
    $tokens = $null; $parseErrors = $null
    $null = [Management.Automation.Language.Parser]::ParseFile($file.FullName, [ref]$tokens, [ref]$parseErrors)
    Assert-Fixture ($parseErrors.Count -eq 0) ('PowerShell parser rejected ' + $file.Name)
}
$workflowPath = Join-Path $PSScriptRoot '..\.github\workflows\windows.yml'
if (Test-Path -LiteralPath $workflowPath) {
    $workflow = Get-Content -LiteralPath $workflowPath -Raw
    foreach ($block in [regex]::Matches($workflow, '(?m)^        run: \|\r?\n((?:          .*\r?\n|\r?\n)+)')) {
        $source = [regex]::Replace($block.Groups[1].Value, '(?m)^          ', '')
        $tokens = $null; $parseErrors = $null
        $null = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$parseErrors)
        Assert-Fixture ($parseErrors.Count -eq 0) 'A workflow PowerShell script block failed parsing.'
    }
}
$root = Join-Path ([IO.Path]::GetTempPath()) ('veylurk operations fixture ' + [guid]::NewGuid().ToString('N'))
$package = Join-Path $root 'package with spaces'
$data = Join-Path $package 'data'
[void][IO.Directory]::CreateDirectory($data)
$database = Join-Path $data 'observations.sqlite3'
[IO.File]::WriteAllBytes($database, [Text.Encoding]::UTF8.GetBytes('invented database bytes; no real history'))
[IO.File]::WriteAllText((Join-Path $package 'veylurk.exe'), 'fixture placeholder; never executed')
Save-VeylurkCredentials $package ([pscustomobject]@{ ClientId = 'dummy-client'; ClientSecret = 'dummy-secret' })
$credentialPath = Get-VeylurkCredentialPath $package
$credentialHash = (Get-FileHash -LiteralPath $credentialPath -Algorithm SHA256).Hash
$backup = Join-Path $root 'backup one'

# Same access/share flags as a Rust file handle; the byte lock overlaps its whole-file lock.
$busy = [IO.File]::Open(($database + '.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite)
try {
    $busy.Lock(0, 1)
    Assert-Rejected { New-VeylurkOfflineBackup $package $backup } 'An active database lock was ignored.'
} finally { $busy.Dispose() }
[IO.File]::WriteAllText(($database + '-wal'), 'pending write-ahead history')
Assert-Rejected { New-VeylurkOfflineBackup $package $backup } 'A nonempty WAL was ignored.'
[IO.File]::WriteAllBytes(($database + '-wal'), [byte[]]@())
New-VeylurkOfflineBackup $package $backup
$manifest = Test-VeylurkBackup $backup
Assert-Fixture ($manifest.files.Count -eq 2) 'Backup omitted an allowed data file.'
Assert-Fixture ((Get-FileHash -LiteralPath (Join-Path $backup 'twitch-credentials.dpapi') -Algorithm SHA256).Hash -ceq $credentialHash) 'Backup changed encrypted credentials.'
Assert-Rejected { New-VeylurkOfflineBackup $package $backup } 'Backup overwrote existing history.'
Assert-Rejected { Restore-VeylurkOfflineBackup $backup (Join-Path $backup 'nested data') } 'Restore modified its own backup directory.'
$restored = Join-Path $root 'restored data'
Restore-VeylurkOfflineBackup $backup $restored
Assert-Fixture ((Get-FileHash -LiteralPath (Join-Path $restored 'observations.sqlite3') -Algorithm SHA256).Hash -ceq (Get-FileHash -LiteralPath $database -Algorithm SHA256).Hash) 'Restored history differs.'
Assert-Fixture ((Get-FileHash -LiteralPath (Join-Path $restored 'twitch-credentials.dpapi') -Algorithm SHA256).Hash -ceq $credentialHash) 'Restore changed ciphertext.'
Assert-Rejected { Restore-VeylurkOfflineBackup $backup $restored } 'Restore overwrote a data directory.'
[IO.File]::AppendAllText((Join-Path $backup 'observations.sqlite3'), 'tampering')
Assert-Rejected { Restore-VeylurkOfflineBackup $backup (Join-Path $root 'rejected restore') } 'Restore accepted a hash mismatch.'
Assert-Fixture (-not (Test-Path -LiteralPath (Join-Path $root 'rejected restore'))) 'Invalid restore created target data.'

$badBackup = Join-Path $root 'path traversal fixture'
[void][IO.Directory]::CreateDirectory($badBackup)
@{ version = 1; files = @(@{ name = '..\observations.sqlite3'; bytes = 1; sha256 = ('0' * 64) }) } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $badBackup 'manifest.json') -Encoding UTF8
Assert-Rejected { Test-VeylurkBackup $badBackup } 'Manifest traversal was accepted.'
# Rotation retains only one bounded previous lifecycle log, without raw app output.
[IO.File]::WriteAllText((Join-Path $data 'background.log'), ('x' * 262145))
Write-VeylurkLifecycle $package 'stopped' 0
Assert-Fixture ((Get-Item -LiteralPath (Join-Path $data 'background.log')).Length -lt 200) 'Lifecycle log did not rotate.'
Assert-Fixture (Test-Path -LiteralPath (Join-Path $data 'background.log.previous')) 'Lifecycle rotation lost previous log.'

# Mock task cmdlets in this script scope. No real task is read, registered, or started.
$script:existingTask = $null
$script:registeredTask = $null
function Get-ScheduledTask { param($TaskName, $ErrorAction) return $script:existingTask }
function New-ScheduledTaskAction { param($Execute, $Argument, $WorkingDirectory) [pscustomobject]@{ Execute = $Execute; Arguments = $Argument; WorkingDirectory = $WorkingDirectory } }
function New-ScheduledTaskTrigger { param([switch]$AtLogOn, $User) [pscustomobject]@{ AtLogOn = [bool]$AtLogOn; User = $User } }
function New-ScheduledTaskPrincipal { param($UserId, $LogonType, $RunLevel) [pscustomobject]@{ UserId = $UserId; LogonType = $LogonType; RunLevel = $RunLevel } }
function New-ScheduledTaskSettingsSet {
    param($RestartCount, $RestartInterval, $ExecutionTimeLimit, $MultipleInstances, [switch]$StartWhenAvailable, [switch]$AllowStartIfOnBatteries, [switch]$DontStopIfGoingOnBatteries, [switch]$Hidden)
    [pscustomobject]@{ RestartCount = $RestartCount; RestartInterval = $RestartInterval; ExecutionTimeLimit = $ExecutionTimeLimit; MultipleInstances = $MultipleInstances; Hidden = [bool]$Hidden }
}
function Register-ScheduledTask { param($TaskName, $Action, $Trigger, $Principal, $Settings, $Description, [switch]$Force) $script:registeredTask = [pscustomobject]@{ TaskName = $TaskName; Actions = @($Action); Trigger = $Trigger; Principal = $Principal; Settings = $Settings } }
$name = Register-VeylurkStartup $package 9123
Assert-Fixture ($name -ceq (Get-VeylurkTaskName $package)) 'Startup task identity is unstable.'
Assert-Fixture ($script:registeredTask.Settings.RestartCount -eq 3 -and $script:registeredTask.Settings.MultipleInstances -eq 'IgnoreNew' -and $script:registeredTask.Settings.Hidden) 'Task supervision settings differ.'
Assert-Fixture ($script:registeredTask.Principal.LogonType -eq 'Interactive' -and $script:registeredTask.Principal.RunLevel -eq 'Limited') 'Startup requested elevation or lost the user session.'
Assert-Fixture ($script:registeredTask.Actions[0].Arguments.Contains('-WindowStyle Hidden') -and $script:registeredTask.Actions[0].Arguments.Contains(' -Port 9123')) 'Startup command lost hidden windows or selected port.'
$script:existingTask = $script:registeredTask
$null = Get-VeylurkOwnedTask $package
$script:existingTask.Actions[0].Arguments = '-File another-script.ps1'
Assert-Rejected { Register-VeylurkStartup $package 9123 } 'An unrelated startup task was overwritten.'

# Fake process and listener, plus a mock loopback API. No HTTP connection is made.
$script:runtime = [pscustomobject]@{ instance_id = 'fixture-instance'; pid = 1234; port = 9123; executable = ('\\?\' + (Join-Path $package 'veylurk.exe')); database = ('\\?\' + $database) }
$script:app = [pscustomobject]@{ Id = 1234; Path = (Join-Path $package 'veylurk.exe'); Exited = $false; Disposed = $false; HandleRead = $false; ExitCode = 0 }
$script:app | Add-Member ScriptProperty Handle { $this.HandleRead = $true; return [IntPtr]1234 }
$script:app | Add-Member ScriptMethod WaitForExit { param($Milliseconds) $this.Exited = $true; return $true }
$script:app | Add-Member ScriptMethod Dispose { $this.Disposed = $true }
$script:listenerPid = 1234
$script:postCount = 0
function Get-Process { param($Id, $ErrorAction) return $script:app }
function Get-NetTCPConnection { param($LocalAddress, $LocalPort, $State, $ErrorAction) return [pscustomobject]@{ OwningProcess = $script:listenerPid } }
function Invoke-RestMethod {
    param($Uri, $TimeoutSec, $Method, $Headers, $ContentType, $Body)
    if ($Method -eq 'Post') {
        $script:postCount++
        Assert-Fixture $script:app.HandleRead 'Shutdown posted before retaining the process handle.'
        Assert-Fixture ($Uri -eq 'http://127.0.0.1:9123/api/shutdown' -and $Headers.Origin -eq 'http://127.0.0.1:9123' -and $ContentType -eq 'application/json') 'Shutdown request lost origin or JSON guards.'
        Assert-Fixture (($Body | ConvertFrom-Json).instance_id -ceq $script:runtime.instance_id) 'Shutdown did not bind the selected instance.'
        return [pscustomobject]@{ state = 'stopping'; instance_id = $script:runtime.instance_id }
    }
    return $script:runtime
}
Stop-VeylurkGracefully $package 9123 1
Assert-Fixture ($script:postCount -eq 1 -and $script:app.Exited -and $script:app.Disposed) 'Shutdown did not wait for the selected app.'
$script:app.Exited = $false
$script:app.Disposed = $false
$script:app.HandleRead = $false
$script:app.ExitCode = 1
$shutdownFailure = $null
try { Stop-VeylurkGracefully $package 9123 1 } catch { $shutdownFailure = $_.Exception.Message }
Assert-Fixture ($null -ne $shutdownFailure -and $shutdownFailure.Contains('History and WAL were retained')) 'Shutdown claimed success after a failed checkpoint exit.'
Assert-Fixture ($script:postCount -eq 2 -and $script:app.Exited -and $script:app.Disposed) 'Failed shutdown did not wait and release the retained process.'
$script:app.ExitCode = $null
Assert-Rejected { Stop-VeylurkGracefully $package 9123 1 } 'Shutdown accepted an unavailable process exit code.'
Assert-Fixture ($script:postCount -eq 3) 'Unavailable exit-code fixture did not reach shutdown.'
$script:app.ExitCode = 0
$script:listenerPid = 5678
Assert-Rejected { Stop-VeylurkGracefully $package 9123 1 } 'Shutdown accepted a different listener process.'
Assert-Fixture ($script:postCount -eq 3) 'Shutdown posted before listener verification.'
$script:listenerPid = 1234
$script:runtime.database = Join-Path $root 'other.sqlite3'
Assert-Rejected { Stop-VeylurkGracefully $package 9123 1 } 'Shutdown accepted a different database.'
Assert-Fixture ($script:postCount -eq 3) 'Shutdown posted before database verification.'
# Temporary invented fixture files are retained; no real history or startup tasks are touched.
Write-Output 'Operations fixtures passed: offline locks/WAL, encrypted backup/restore, hash/path rejection, log rotation, mocked startup and verified graceful shutdown.'
