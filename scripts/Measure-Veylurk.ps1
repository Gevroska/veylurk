#requires -Version 5.1
[CmdletBinding()]
param(
    [string]$PackageDirectory = $PSScriptRoot,
    [ValidateRange(1, 65535)][int]$Port = 8794,
    [ValidateRange(1, 86400)][int]$DurationSeconds = 3600,
    [ValidateRange(2, 86400)][int]$IntervalSeconds = 15,
    [string]$OutputDirectory
)

# Dot-sourcing defines helpers only. No app, task, database, or network is touched.
. (Join-Path $PSScriptRoot 'Veylurk-Operations.ps1')

function Get-VeylurkMeasurementClock { param($Clock) return $Clock.Elapsed.TotalMilliseconds }
function Wait-VeylurkMeasurement { param([int]$Milliseconds) if ($Milliseconds -gt 0) { Start-Sleep -Milliseconds $Milliseconds } }
function Get-VeylurkMeasurementValue {
    param($Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -ne $property) { return $property.Value }
    return $null
}
function ConvertTo-VeylurkMeasurementNumber {
    param($Value)
    if ($null -eq $Value -or $Value -is [bool] -or $Value -is [string]) { return $null }
    try {
        $number = [decimal]$Value
        if ($number -lt 0 -or $number -gt [long]::MaxValue -or [decimal]::Truncate($number) -ne $number) { return $null }
        return [long]$number
    } catch { return $null }
}
function Get-VeylurkMeasurementState {
    param($Value, [string[]]$Allowed)
    if ($Value -is [string] -and $Allowed -ccontains $Value) { return $Value }
    return 'unknown'
}

function Invoke-VeylurkMeasurementRequest {
    param([int]$Port, [ValidateSet('runtime', 'status')][string]$Endpoint, $Clock, [double]$DeadlineMs)
    $begin = Get-VeylurkMeasurementClock $Clock
    $request = $null; $response = $null; $stream = $null; $memory = $null
    $requestDeadline = [math]::Min($DeadlineMs, $begin + 2000)
    $result = 'unavailable'; $value = $null
    try {
        $remaining = [math]::Floor($requestDeadline - $begin)
        if ($remaining -lt 1) { $result = 'deadline' }
        else {
            $request = [Net.HttpWebRequest]::Create("http://127.0.0.1:$Port/api/$Endpoint")
            $request.Proxy = $null
            $request.UseDefaultCredentials = $false
            $request.AllowAutoRedirect = $false
            $request.Method = 'GET'
            $request.Timeout = [int][math]::Min(2000, $remaining)
            $request.ReadWriteTimeout = $request.Timeout
            $request.KeepAlive = $false
            $response = $request.GetResponse()
            if ([int]$response.StatusCode -ne 200) { $result = 'http_error' }
            else {
                $stream = $response.GetResponseStream()
                $memory = New-Object IO.MemoryStream
                $buffer = New-Object byte[] 4096
                $result = 'ok'
                while ($true) {
                    $remaining = [math]::Floor($requestDeadline - (Get-VeylurkMeasurementClock $Clock))
                    if ($remaining -lt 1) { $result = 'deadline'; break }
                    $stream.ReadTimeout = [int][math]::Min(2000, $remaining)
                    $count = $stream.Read($buffer, 0, $buffer.Length)
                    if ($count -eq 0) { break }
                    if ($memory.Length + $count -gt 262144) { $result = 'response_limit'; break }
                    $memory.Write($buffer, 0, $count)
                }
                if ($result -eq 'ok') {
                    try { $value = [Text.Encoding]::UTF8.GetString($memory.ToArray()) | ConvertFrom-Json -ErrorAction Stop }
                    catch { $result = 'invalid_json'; $value = $null }
                }
            }
        }
    } catch {
        # Never persist an exception, response body, URL, or arbitrary server text.
        if ($_.Exception -is [Net.WebException] -and $null -ne $_.Exception.Response) { $_.Exception.Response.Dispose() }
        $result = 'unavailable'; $value = $null
    } finally {
        if ($null -ne $stream) { $stream.Dispose() }
        if ($null -ne $memory) { $memory.Dispose() }
        if ($null -ne $response) { $response.Dispose() }
        if ($null -ne $request) { $request.Abort() }
    }
    return [pscustomobject]@{ result = $result; duration_ms = [math]::Round((Get-VeylurkMeasurementClock $Clock) - $begin, 3); value = $value }
}

function Test-VeylurkMeasurementIdentity {
    param($Runtime, $Identity, [string]$PackageDirectory, [int]$Port)
    try {
        return ((Get-VeylurkMeasurementValue $Runtime 'instance_id') -is [string] -and
            (Get-VeylurkMeasurementValue $Runtime 'instance_id') -ceq $Identity.instance_id -and
            (Get-VeylurkMeasurementValue $Runtime 'pid') -eq $Identity.process_id -and
            (Get-VeylurkMeasurementValue $Runtime 'started_at') -ceq $Identity.started_at -and
            (Get-VeylurkMeasurementValue $Runtime 'port') -eq $Port -and
            (ConvertTo-VeylurkNormalPath (Get-VeylurkMeasurementValue $Runtime 'executable')) -ieq (Join-Path $PackageDirectory 'veylurk.exe') -and
            (ConvertTo-VeylurkNormalPath (Get-VeylurkMeasurementValue $Runtime 'database')) -ieq (Join-Path $PackageDirectory 'data\observations.sqlite3'))
    } catch { return $false }
}

function Get-VeylurkMeasurementProcess {
    param($App, [long]$StartTicks, $Clock)
    try {
        $App.Refresh()
        if ($App.HasExited -or $App.StartTime.ToUniversalTime().Ticks -ne $StartTicks) { return [pscustomobject]@{ result = 'exited_or_replaced'; value = $null } }
        $cpu = $App.TotalProcessorTime.TotalMilliseconds
        $working = $App.WorkingSet64; $private = $App.PrivateMemorySize64
        if ($cpu -lt 0 -or $working -lt 0 -or $private -lt 0) { throw 'Invalid metrics.' }
        return [pscustomobject]@{ result = 'ok'; value = [pscustomobject]@{ elapsed_ms = (Get-VeylurkMeasurementClock $Clock); cpu_ms = $cpu; working_set_bytes = $working; private_bytes = $private } }
    } catch { return [pscustomobject]@{ result = 'unavailable'; value = $null } }
}

function Get-VeylurkMeasurementCpuDelta {
    param($Previous, $Current, [int]$LogicalProcessors)
    $result = [ordered]@{ result = 'baseline'; elapsed_ms = $null; cpu_ms = $null; one_core_percent = $null; machine_percent = $null }
    if ($null -ne $Previous) {
        $elapsed = $Current.elapsed_ms - $Previous.elapsed_ms
        $cpu = $Current.cpu_ms - $Previous.cpu_ms
        if ($elapsed -le 0 -or $cpu -lt 0) { $result.result = 'counter_reset_or_clock_invalid' }
        else {
            $result.result = 'ok'; $result.elapsed_ms = [math]::Round($elapsed, 3); $result.cpu_ms = [math]::Round($cpu, 3)
            $result.one_core_percent = [math]::Round(100 * $cpu / $elapsed, 4)
            $result.machine_percent = [math]::Round(100 * $cpu / $elapsed / $LogicalProcessors, 4)
        }
    }
    return [pscustomobject]$result
}

function ConvertTo-VeylurkMeasurementAggregate {
    param($Status)
    $result = [ordered]@{ result = 'ok' }
    foreach ($name in @('observation_count', 'session_count', 'enabled_channels', 'connected_channels', 'storage_bytes', 'disk_available_bytes')) {
        $result[$name] = ConvertTo-VeylurkMeasurementNumber (Get-VeylurkMeasurementValue $Status $name)
        if ($null -eq $result[$name]) { $result.result = 'partial' }
    }
    $result.state = Get-VeylurkMeasurementState (Get-VeylurkMeasurementValue $Status 'state') @('starting', 'idle', 'collecting', 'membership_unavailable', 'joining', 'connecting', 'storage_error', 'backoff', 'stopped')
    $result.storage_health = Get-VeylurkMeasurementState (Get-VeylurkMeasurementValue $Status 'storage_health') @('available', 'unavailable')
    if ($result.state -eq 'unknown' -or $result.storage_health -eq 'unknown') { $result.result = 'partial' }
    return [pscustomobject]$result
}

function Get-VeylurkMeasurementCounterDelta {
    param($Previous, $Current)
    $result = [ordered]@{}
    foreach ($name in @('observation_count', 'session_count')) {
        $value = $null; $quality = 'unavailable'
        if ($null -ne $Current -and $null -ne $Current.$name) {
            $quality = 'baseline'
            if ($null -ne $Previous -and $null -ne $Previous.$name) {
                if ($Current.$name -lt $Previous.$name) { $quality = 'counter_reset' }
                else { $quality = 'ok'; $value = $Current.$name - $Previous.$name }
            }
        }
        $result[$name] = [pscustomobject]@{ result = $quality; delta = $value }
    }
    return [pscustomobject]$result
}

function New-VeylurkMeasurementDirectory {
    param([string]$OutputDirectory, [string]$PackageDirectory)
    if ([string]::IsNullOrWhiteSpace($OutputDirectory)) { throw 'Specify a fresh report directory outside the package.' }
    $path = [IO.Path]::GetFullPath($OutputDirectory).TrimEnd('\')
    Assert-VeylurkOrdinaryPath $path
    if (Test-Path -LiteralPath $path) { throw 'The report directory already exists. Choose a fresh destination.' }
    if ($path -ieq $PackageDirectory -or $path.StartsWith(($PackageDirectory.TrimEnd('\') + '\'), [StringComparison]::OrdinalIgnoreCase)) { throw 'Reports must be outside the application package.' }
    $parent = [IO.Path]::GetDirectoryName($path)
    if (-not [IO.Directory]::Exists($parent)) { throw 'The report parent directory must already exist.' }
    $stage = Join-Path $parent ('.veylurk-measurement-' + [guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($stage)
    # Directory.Move refuses an existing destination, including a concurrent creator.
    try { [IO.Directory]::Move($stage, $path) }
    catch { throw 'Could not reserve a fresh report directory.' }
    return $path
}

function Write-VeylurkMeasurementSample {
    param($Stream, $Sample, [long]$MaxBytes)
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Sample | ConvertTo-Json -Depth 8 -Compress) + "`n")
    if ($bytes.Length -gt 8192 -or $Stream.Length + $bytes.Length -gt $MaxBytes - 32768) { return $false }
    $Stream.Write($bytes, 0, $bytes.Length); $Stream.Flush()
    return $true
}

function Invoke-VeylurkMeasurement {
    param(
        [string]$PackageDirectory, [ValidateRange(1, 65535)][int]$Port,
        [ValidateRange(1, 86400)][int]$DurationSeconds,
        [ValidateRange(2, 86400)][int]$IntervalSeconds, [string]$OutputDirectory,
        [ValidateRange(1, 43200)][int]$MaxSamples = 43200,
        [ValidateRange(32768, 134217728)][long]$MaxOutputBytes = 134217728
    )
    $PackageDirectory = Get-VeylurkPackagePath $PackageDirectory
    Assert-VeylurkOrdinaryPath (Join-Path $PackageDirectory 'veylurk.exe')
    Assert-VeylurkOrdinaryPath (Join-Path $PackageDirectory 'data\observations.sqlite3')
    $clock = [Diagnostics.Stopwatch]::StartNew(); $deadline = $DurationSeconds * 1000.0
    $created = [DateTime]::UtcNow.ToString('o')
    $initial = Invoke-VeylurkMeasurementRequest $Port 'runtime' $clock $deadline
    if ($initial.result -ne 'ok') { throw 'The initial local runtime is unavailable. No measurement was started.' }
    $runtime = $initial.value
    $instance = Get-VeylurkMeasurementValue $runtime 'instance_id'
    $started = Get-VeylurkMeasurementValue $runtime 'started_at'
    $processId = ConvertTo-VeylurkMeasurementNumber (Get-VeylurkMeasurementValue $runtime 'pid')
    $parsed = [DateTimeOffset]::MinValue
    if ($instance -isnot [string] -or $instance.Length -gt 128 -or $instance -notmatch '^[a-zA-Z0-9_.:+-]{1,128}$' -or
        $started -isnot [string] -or $started.Length -gt 64 -or -not [DateTimeOffset]::TryParse($started, [ref]$parsed) -or
        $null -eq $processId -or $processId -lt 1 -or $processId -gt [int]::MaxValue -or
        $instance -cne ([string][int]$processId + '-' + $started)) { throw 'The runtime identity is invalid. No measurement was started.' }
    $app = $null; $stream = $null
    try {
        try {
            $app = Assert-VeylurkRuntime $PackageDirectory $Port $runtime
            $null = $app.Handle
            $startTicks = $app.StartTime.ToUniversalTime().Ticks
        } catch { throw 'The runtime does not match the selected package, database, and process. No measurement was started.' }
        $identity = [pscustomobject]@{ instance_id = $instance; process_id = [int]$processId; started_at = $parsed.ToUniversalTime().ToString('o') }
        # Preserve the exact source stamp for equality; do not persist its raw formatting.
        $binding = [pscustomobject]@{ instance_id = $instance; process_id = [int]$processId; started_at = $started }
        if (-not (Test-VeylurkMeasurementIdentity $runtime $binding $PackageDirectory $Port)) { throw 'Invalid runtime binding.' }
        $output = New-VeylurkMeasurementDirectory $OutputDirectory $PackageDirectory
        $samplesPath = Join-Path $output 'samples.jsonl'
        Assert-VeylurkOrdinaryPath $samplesPath
        $stream = [IO.File]::Open($samplesPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        $logical = [Environment]::ProcessorCount
        $count = 0; $dropped = 0; $reason = 'duration'; $previousCpu = $null; $previousAggregate = $null; $previousStart = $null
        $outcomes = @{}; $maxGap = 0.0; $maxWorking = $null; $maxPrivate = $null
        while ((Get-VeylurkMeasurementClock $clock) -lt $deadline -and $count -lt $MaxSamples) {
            $begin = Get-VeylurkMeasurementClock $clock
            $gap = $null
            if ($null -ne $previousStart) { $gap = $begin - $previousStart; $maxGap = [math]::Max($maxGap, $gap) }
            $sample = [ordered]@{ sequence = $count; timestamp_utc = [DateTime]::UtcNow.ToString('o'); elapsed_ms = [math]::Round($begin, 3); since_previous_sample_ms = $gap; result = 'unavailable'; runtime_before = $null; status_request = $null; runtime_after = $null; aggregate = $null; counters = $null; process = $null; cpu = $null }
            $before = Invoke-VeylurkMeasurementRequest $Port 'runtime' $clock $deadline
            $sample.runtime_before = [pscustomobject]@{ result = $before.result; duration_ms = $before.duration_ms }
            if ($before.result -eq 'ok') {
                if (-not (Test-VeylurkMeasurementIdentity $before.value $binding $PackageDirectory $Port)) { $sample.result = 'instance_changed' }
                else {
                    $status = Invoke-VeylurkMeasurementRequest $Port 'status' $clock $deadline
                    $sample.status_request = [pscustomobject]@{ result = $status.result; duration_ms = $status.duration_ms }
                    $after = Invoke-VeylurkMeasurementRequest $Port 'runtime' $clock $deadline
                    $sample.runtime_after = [pscustomobject]@{ result = $after.result; duration_ms = $after.duration_ms }
                    if ($after.result -ne 'ok') { $sample.result = 'identity_unavailable' }
                    elseif (-not (Test-VeylurkMeasurementIdentity $after.value $binding $PackageDirectory $Port)) { $sample.result = 'instance_changed' }
                    else {
                        $sample.result = 'status_unavailable'
                        if ($status.result -eq 'ok') { $sample.aggregate = ConvertTo-VeylurkMeasurementAggregate $status.value; $sample.result = $sample.aggregate.result }
                        $metrics = Get-VeylurkMeasurementProcess $app $startTicks $clock
                        $sample.process = $metrics
                        if ($metrics.result -eq 'ok') {
                            $sample.cpu = Get-VeylurkMeasurementCpuDelta $previousCpu $metrics.value $logical
                            $previousCpu = $metrics.value
                            $maxWorking = [math]::Max([double]$maxWorking, $metrics.value.working_set_bytes)
                            $maxPrivate = [math]::Max([double]$maxPrivate, $metrics.value.private_bytes)
                        } else { $previousCpu = $null }
                        $sample.counters = Get-VeylurkMeasurementCounterDelta $previousAggregate $sample.aggregate
                        $previousAggregate = $sample.aggregate
                    }
                }
            }
            if ($sample.result -eq 'instance_changed' -or $sample.result -eq 'identity_unavailable' -or $sample.result -eq 'unavailable') { $previousCpu = $null; $previousAggregate = $null }
            $sample.duration_ms = [math]::Round((Get-VeylurkMeasurementClock $clock) - $begin, 3)
            $sample.interval_overrun_ms = [math]::Max(0, $sample.duration_ms - $IntervalSeconds * 1000)
            if (-not (Write-VeylurkMeasurementSample $stream ([pscustomobject]$sample) $MaxOutputBytes)) { $reason = 'output_limit'; $dropped++; break }
            if (-not $outcomes.ContainsKey($sample.result)) { $outcomes[$sample.result] = 0 }
            $outcomes[$sample.result]++; $count++; $previousStart = $begin
            $now = Get-VeylurkMeasurementClock $clock
            $wait = [math]::Min($deadline - $now, $IntervalSeconds * 1000 - ($now - $begin))
            if ($wait -gt 0) { Wait-VeylurkMeasurement ([int][math]::Floor($wait)) }
        }
        if ($count -ge $MaxSamples -and (Get-VeylurkMeasurementClock $clock) -lt $deadline) { $reason = 'sample_limit' }
        $summary = [ordered]@{ schema_version = 1; created_at_utc = $created; finished_at_utc = [DateTime]::UtcNow.ToString('o'); identity = $identity; requested_duration_seconds = $DurationSeconds; interval_seconds = $IntervalSeconds; elapsed_ms = [math]::Round((Get-VeylurkMeasurementClock $clock), 3); initial_runtime_request_ms = $initial.duration_ms; stop_reason = $reason; samples = $count; samples_not_written_due_to_limit = $dropped; sample_outcomes = $outcomes; maximum_sample_start_gap_ms = $maxGap; peak_sampled_working_set_bytes = $maxWorking; peak_sampled_private_bytes = $maxPrivate; logical_processors = $logical; cpu_basis = 'Process CPU delta / actual monotonic snapshot elapsed; one_core_percent can exceed 100; machine_percent divides by logical_processors.'; coverage = 'Tool polling only. Gaps and successful samples do not establish continuous app, IRC membership, account presence, or viewing coverage.'; limits = @{ duration_seconds = 86400; samples = $MaxSamples; output_bytes = $MaxOutputBytes; sample_bytes = 8192; response_bytes = 262144 } }
        $summaryPath = Join-Path $output 'summary.json'
        Assert-VeylurkOrdinaryPath $summaryPath
        $bytes = [Text.Encoding]::UTF8.GetBytes(($summary | ConvertTo-Json -Depth 8) + "`n")
        if ($bytes.Length -gt 32768 -or $stream.Length + $bytes.Length -gt $MaxOutputBytes) { throw 'The bounded summary could not be written.' }
        $summaryStream = [IO.File]::Open($summaryPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        try { $summaryStream.Write($bytes, 0, $bytes.Length) } finally { $summaryStream.Dispose() }
        return [pscustomobject]$summary
    } finally { if ($null -ne $stream) { $stream.Dispose() }; if ($null -ne $app) { $app.Dispose() }; $clock.Stop() }
}

if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    try { Invoke-VeylurkMeasurement -PackageDirectory $PackageDirectory -Port $Port -DurationSeconds $DurationSeconds -IntervalSeconds $IntervalSeconds -OutputDirectory $OutputDirectory }
    catch { throw 'Measurement stopped. Check the package identity and fresh output destination; any written samples remain in the report directory.' }
}
