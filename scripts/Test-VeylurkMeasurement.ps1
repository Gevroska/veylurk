#requires -Version 5.1
# Invented files, process objects and loopback responses only; no app is contacted.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Measure-Veylurk.ps1')
function Assert-MeasurementFixture { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }
function Assert-MeasurementRejected { param([scriptblock]$Action, [string]$Message) $failed = $false; try { & $Action } catch { $failed = $true }; Assert-MeasurementFixture $failed $Message }
foreach ($name in @('Measure-Veylurk.ps1', 'Test-VeylurkMeasurement.ps1')) {
    $tokens = $null; $errors = $null
    $null = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $name), [ref]$tokens, [ref]$errors)
    Assert-MeasurementFixture ($errors.Count -eq 0) ('PowerShell parsing failed: ' + $name)
}
$root = Join-Path ([IO.Path]::GetTempPath()) ('veylurk measurement fixture ' + [guid]::NewGuid().ToString('N'))
$package = Join-Path $root 'package with spaces'
[void][IO.Directory]::CreateDirectory((Join-Path $package 'data'))
[IO.File]::WriteAllText((Join-Path $package 'veylurk.exe'), 'invented placeholder; never executed')
$database = Join-Path $package 'data\observations.sqlite3'
[IO.File]::WriteAllText($database, 'invented database sentinel; never opened by measurement')
$databaseHash = (Get-FileHash -LiteralPath $database -Algorithm SHA256).Hash

# Replace every external observation seam. The real ownership helper still checks
# exact package paths, process path, port and listener PID using invented objects.
$script:tick = 0.0; $script:requests = 0; $script:processReads = 0; $script:scenario = 'healthy'
$script:runtime = [pscustomobject]@{ instance_id = '1234-2026-01-01T00:00:00.000Z'; pid = 1234; started_at = '2026-01-01T00:00:00.000Z'; port = 8794; executable = (Join-Path $package 'veylurk.exe'); database = $database }
$script:app = [pscustomobject]@{ Id = 1234; Path = (Join-Path $package 'veylurk.exe'); Handle = [IntPtr]1234; StartTime = [DateTime]::Parse('2026-01-01T00:00:00Z').ToUniversalTime(); HasExited = $false; WorkingSet64 = 1048576L; PrivateMemorySize64 = 2097152L }
$script:app | Add-Member ScriptProperty TotalProcessorTime { $script:processReads++; return [TimeSpan]::FromMilliseconds($script:tick / 4) }
$script:app | Add-Member ScriptMethod Refresh { }
$script:app | Add-Member ScriptMethod Dispose { }
function Get-Process { param($Id, $ErrorAction) return $script:app }
function Get-NetTCPConnection { param($LocalAddress, $LocalPort, $State, $ErrorAction) return [pscustomobject]@{ OwningProcess = 1234 } }
function Get-VeylurkMeasurementClock { param($Clock) return $script:tick }
function Wait-VeylurkMeasurement { param([int]$Milliseconds) $script:tick += $Milliseconds }
function Invoke-VeylurkMeasurementRequest {
    param([int]$Port, [string]$Endpoint, $Clock, [double]$DeadlineMs)
    $script:requests++; $requestDuration = 100
    if ($script:scenario -eq 'slow') { $requestDuration = 1000 }
    $script:tick += $requestDuration
    $result = 'ok'; $value = $null
    if ($Endpoint -eq 'runtime') {
        $value = $script:runtime.PSObject.Copy()
        if ($script:scenario -eq 'replacement' -and $script:requests -ge 5) { $value.instance_id = '9876-2026-01-01T00:00:01.000Z'; $value.pid = 9876 }
        if ($script:scenario -eq 'replacement_mid_status' -and $script:requests -ge 4) { $value.instance_id = '9876-2026-01-01T00:00:01.000Z'; $value.pid = 9876 }
        if ($script:scenario -eq 'start_changed' -and $script:requests -ge 5) { $value.started_at = '2026-01-01T00:00:01.000Z' }
        if ($script:scenario -eq 'absent' -and $script:requests -ge 5) { $result = 'unavailable'; $value = $null }
        if ($script:scenario -eq 'wrong_database') { $value.database = Join-Path $root 'unrelated.sqlite3' }
        if ($script:scenario -eq 'wrong_executable') { $value.executable = Join-Path $root 'unrelated.exe' }
        if ($script:scenario -eq 'wrong_port') { $value.port = 9123 }
        if ($script:scenario -eq 'initial_absent') { $result = 'unavailable'; $value = $null }
    } else {
        $value = [pscustomobject]@{ observation_count = (100 + $script:requests); session_count = 2; enabled_channels = 2; connected_channels = 1; state = 'collecting'; storage_bytes = 4096; disk_available_bytes = 1073741824; storage_health = 'available'; last_error = 'secret-token https://private.example/'; storage_path = 'private-path'; rooms = @(@{ channel = 'private-account'; last_error = 'secret-token' }) }
        if ($script:scenario -eq 'backoff') { $value.state = 'backoff'; $value.connected_channels = 0 }
        if ($script:scenario -eq 'status_absent' -and $script:requests -ge 5) { $result = 'unavailable'; $value = $null }
    }
    return [pscustomobject]@{ result = $result; duration_ms = $requestDuration; value = $value }
}
function Invoke-MeasurementFixture {
    param([string]$Scenario, [string]$Destination, [int]$Seconds = 7, [int]$MaxSamples = 43200, [long]$MaxOutputBytes = 134217728)
    $script:scenario = $Scenario; $script:tick = 0; $script:requests = 0; $script:processReads = 0
    Invoke-VeylurkMeasurement -PackageDirectory $package -Port 8794 -DurationSeconds $Seconds -IntervalSeconds 2 -OutputDirectory $Destination -MaxSamples $MaxSamples -MaxOutputBytes $MaxOutputBytes
}

$healthyPath = Join-Path $root 'healthy'
$healthy = Invoke-MeasurementFixture 'healthy' $healthyPath
$samples = @(Get-Content -LiteralPath (Join-Path $healthyPath 'samples.jsonl') | ForEach-Object { $_ | ConvertFrom-Json })
Assert-MeasurementFixture ($samples.Count -eq 4 -and $healthy.samples -eq 4) 'Finite sampling or summary count differs.'
Assert-MeasurementFixture ($samples[1].cpu.one_core_percent -eq 25 -and $samples[1].cpu.elapsed_ms -eq 2000) 'CPU denominator used configured sleep or request duration instead of actual snapshot elapsed.'
Assert-MeasurementFixture ($samples[1].cpu.machine_percent -eq [math]::Round(25.0 / [Environment]::ProcessorCount, 4)) 'Machine CPU normalization differs.'
Assert-MeasurementFixture ($samples[0].runtime_before.duration_ms -eq 100 -and $samples[0].duration_ms -eq 300) 'Actual request/sample times were omitted.'
Assert-MeasurementFixture ($samples[1].counters.observation_count.delta -eq 3) 'Aggregate counter deltas differ.'
$slowPath = Join-Path $root 'slow'
$slow = Invoke-MeasurementFixture 'slow' $slowPath
$slowRows = @(Get-Content -LiteralPath (Join-Path $slowPath 'samples.jsonl') | ForEach-Object { $_ | ConvertFrom-Json })
Assert-MeasurementFixture ($slowRows.Count -eq 2 -and $slowRows[1].since_previous_sample_ms -eq 3000 -and $slowRows[1].cpu.elapsed_ms -eq 3000 -and $slowRows[1].cpu.one_core_percent -eq 25 -and $slowRows[1].interval_overrun_ms -eq 1000) 'Slow requests hid tool gaps or distorted CPU rates.'
$text = (Get-Content -LiteralPath (Join-Path $healthyPath 'samples.jsonl') -Raw) + (Get-Content -LiteralPath (Join-Path $healthyPath 'summary.json') -Raw)
foreach ($secret in @('secret-token', 'private-account', 'private-path', 'https://private.example', $database, $package)) { Assert-MeasurementFixture (-not $text.Contains($secret)) 'A report included an excluded field or path.' }
$backoffPath = Join-Path $root 'backoff'
$backoff = Invoke-MeasurementFixture 'backoff' $backoffPath
$backoffRows = @(Get-Content -LiteralPath (Join-Path $backoffPath 'samples.jsonl') | ForEach-Object { $_ | ConvertFrom-Json })
Assert-MeasurementFixture ($backoff.sample_outcomes.ok -eq 4 -and $backoffRows[1].aggregate.state -eq 'backoff' -and $backoffRows[1].aggregate.connected_channels -eq 0 -and $backoffRows[1].aggregate.result -eq 'ok') 'Expected collector backoff was classified as an unknown or incomplete metric.'
Assert-MeasurementFixture (-not (Get-Content -LiteralPath (Join-Path $backoffPath 'samples.jsonl') -Raw).Contains('secret-token')) 'Backoff exposed an arbitrary collector error.'

foreach ($scenario in @('replacement', 'start_changed', 'absent', 'status_absent')) {
    $path = Join-Path $root $scenario
    $summary = Invoke-MeasurementFixture $scenario $path
    $rows = @(Get-Content -LiteralPath (Join-Path $path 'samples.jsonl') | ForEach-Object { $_ | ConvertFrom-Json })
    Assert-MeasurementFixture ($rows.Count -eq 4 -and $summary.stop_reason -eq 'duration') 'An API failure prematurely ended the finite trial.'
    if ($scenario -eq 'status_absent') {
        Assert-MeasurementFixture ($rows[1].result -eq 'status_unavailable' -and $null -eq $rows[1].aggregate -and $rows[1].process.result -eq 'ok') 'Status absence was hidden or lost valid original-process metrics.'
        Assert-MeasurementFixture ($rows[1].status_request.result -eq 'unavailable' -and $rows[1].status_request.duration_ms -eq 100) 'A failed status request lost its actual duration.'
    } else {
        $expected = 'instance_changed'; if ($scenario -eq 'absent') { $expected = 'unavailable' }
        Assert-MeasurementFixture ($rows[1].result -eq $expected -and $null -eq $rows[1].aggregate -and $null -eq $rows[1].process -and $script:processReads -eq 1) 'A replacement/unverified process supplied measurements.'
        if ($scenario -eq 'absent') { Assert-MeasurementFixture ($rows[1].runtime_before.result -eq 'unavailable' -and $rows[1].runtime_before.duration_ms -eq 100) 'A failed runtime request lost its actual duration.' }
    }
}
# A replacement during status must invalidate that response as well.
$path = Join-Path $root 'mid status'
$summary = Invoke-MeasurementFixture 'replacement_mid_status' $path
$rows = @(Get-Content -LiteralPath (Join-Path $path 'samples.jsonl') | ForEach-Object { $_ | ConvertFrom-Json })
Assert-MeasurementFixture ($rows[0].result -eq 'instance_changed' -and $null -eq $rows[0].aggregate -and $null -eq $rows[0].process -and $script:processReads -eq 0) 'A status response spanning a replacement was retained.'
$binding = [pscustomobject]@{ instance_id = $script:runtime.instance_id; process_id = 1234; started_at = $script:runtime.started_at }
$changed = $script:runtime.PSObject.Copy(); $changed.pid = 4321
Assert-MeasurementFixture (-not (Test-VeylurkMeasurementIdentity $changed $binding $package 8794)) 'PID-only replacement was accepted.'
$changed = $script:runtime.PSObject.Copy(); $changed.instance_id = 'another-instance'
Assert-MeasurementFixture (-not (Test-VeylurkMeasurementIdentity $changed $binding $package 8794)) 'Instance-only replacement was accepted.'

Assert-MeasurementRejected { Invoke-MeasurementFixture 'wrong_database' (Join-Path $root 'wrong database') } 'The initial foreign database was accepted.'
Assert-MeasurementRejected { Invoke-MeasurementFixture 'wrong_executable' (Join-Path $root 'wrong executable') } 'The initial foreign executable was accepted.'
Assert-MeasurementRejected { Invoke-MeasurementFixture 'wrong_port' (Join-Path $root 'wrong port') } 'The initial foreign port was accepted.'
Assert-MeasurementRejected { Invoke-MeasurementFixture 'initial_absent' (Join-Path $root 'initial absent') } 'An absent initial runtime was accepted.'
Assert-MeasurementRejected { Invoke-MeasurementFixture 'healthy' $healthyPath } 'An existing report destination was reused.'
Assert-MeasurementRejected { Invoke-MeasurementFixture 'healthy' (Join-Path $package 'data\report') } 'A report was written inside application data.'
Assert-MeasurementRejected { Invoke-VeylurkMeasurement -PackageDirectory $package -Port 8794 -DurationSeconds 86401 -IntervalSeconds 2 -OutputDirectory (Join-Path $root 'too long') } 'The one-day duration bound was ignored.'
Assert-MeasurementRejected { Invoke-VeylurkMeasurement -PackageDirectory $package -Port 8794 -DurationSeconds 3 -IntervalSeconds 1 -OutputDirectory (Join-Path $root 'too frequent') } 'The minimum interval bound was ignored.'
$junction = Join-Path $root 'linked parent'
$null = New-Item -ItemType Junction -Path $junction -Target $package
Assert-MeasurementRejected { Invoke-MeasurementFixture 'healthy' (Join-Path $junction 'report') } 'A report path through a junction was accepted.'

$limited = Invoke-MeasurementFixture 'healthy' (Join-Path $root 'sample limited') 7 1
Assert-MeasurementFixture ($limited.samples -eq 1 -and $limited.stop_reason -eq 'sample_limit') 'Sample count was not bounded.'
$limitedPath = Join-Path $root 'output limited'
$limited = Invoke-MeasurementFixture 'healthy' $limitedPath 7 43200 32768
Assert-MeasurementFixture ($limited.samples -eq 0 -and $limited.samples_not_written_due_to_limit -eq 1 -and $limited.stop_reason -eq 'output_limit') 'Output exhaustion silently dropped a sample.'
$total = (Get-ChildItem -LiteralPath $limitedPath | Measure-Object -Property Length -Sum).Sum
Assert-MeasurementFixture ($total -le 32768) 'The output byte bound was exceeded.'
$memory = New-Object IO.MemoryStream
try {
    $written = Write-VeylurkMeasurementSample $memory ([pscustomobject]@{ untrusted_fixture = ('x' * 9000) }) 134217728
    Assert-MeasurementFixture (-not $written -and $memory.Length -eq 0) 'An oversized sample was partially written.'
} finally { $memory.Dispose() }
$previous = [pscustomobject]@{ elapsed_ms = 500; cpu_ms = 200 }
$current = [pscustomobject]@{ elapsed_ms = 3500; cpu_ms = 500 }
$delta = Get-VeylurkMeasurementCpuDelta $previous $current 4
Assert-MeasurementFixture ($delta.one_core_percent -eq 10 -and $delta.machine_percent -eq 2.5 -and $delta.elapsed_ms -eq 3000) 'CPU did not use the actual monotonic denominator.'
$current.cpu_ms = 100
Assert-MeasurementFixture ((Get-VeylurkMeasurementCpuDelta $previous $current 4).result -eq 'counter_reset_or_clock_invalid') 'A negative CPU delta was accepted.'
$current.cpu_ms = 500; $current.elapsed_ms = 500
Assert-MeasurementFixture ((Get-VeylurkMeasurementCpuDelta $previous $current 4).result -eq 'counter_reset_or_clock_invalid') 'A zero CPU denominator was accepted.'
$counter = Get-VeylurkMeasurementCounterDelta ([pscustomobject]@{ observation_count = 50; session_count = 5 }) ([pscustomobject]@{ observation_count = 1; session_count = 4 })
Assert-MeasurementFixture ($counter.observation_count.result -eq 'counter_reset' -and $null -eq $counter.observation_count.delta) 'A reset was reported as negative activity.'
$bad = ConvertTo-VeylurkMeasurementAggregate ([pscustomobject]@{ state = 'secret-token'; storage_health = 'private-error'; observation_count = 'secret-token'; connected_channels = -1 })
Assert-MeasurementFixture ($bad.result -eq 'partial' -and $bad.state -eq 'unknown' -and $null -eq $bad.observation_count -and $null -eq $bad.connected_channels) 'Untrusted metric strings or negative counts passed through.'
$missing = ConvertTo-VeylurkMeasurementAggregate ([pscustomobject]@{})
Assert-MeasurementFixture ($missing.result -eq 'partial' -and $missing.state -eq 'unknown' -and $missing.storage_health -eq 'unknown' -and $null -eq $missing.observation_count) 'Absent state or numeric fields were silently treated as valid.'
Assert-MeasurementFixture ((Get-VeylurkMeasurementState $null @('collecting')) -eq 'unknown') 'A null state did not have an explicit unknown result.'
Assert-MeasurementFixture ((ConvertTo-VeylurkMeasurementNumber 9007199254740993L) -eq 9007199254740993L) 'Large integer counters lost precision.'
Assert-MeasurementFixture ((Get-FileHash -LiteralPath $database -Algorithm SHA256).Hash -ceq $databaseHash) 'Measurement changed invented database bytes.'
Write-Output 'Measurement fixtures passed: ownership, finite API failures, replacement, actual CPU timing, counter reset, redaction, fresh/reparse destinations, and report bounds. No app, task, HTTP endpoint, or real database was accessed.'
