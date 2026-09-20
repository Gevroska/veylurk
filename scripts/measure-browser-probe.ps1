param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [Parameter(Mandatory = $true)]
    [string[]]$Channel,
    [int]$Samples = 3,
    [int]$IntervalSeconds = 15,
    [int]$TimeoutSeconds = 120
)

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$arguments = [System.Collections.Generic.List[string]]::new()
foreach ($item in $Channel) { $arguments.Add("--channel"); $arguments.Add($item) }
$arguments.Add("--samples"); $arguments.Add([string]$Samples)
$arguments.Add("--interval"); $arguments.Add([string]$IntervalSeconds)
$arguments.Add("--timeout"); $arguments.Add([string]$TimeoutSeconds)

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $resolvedExecutable
foreach ($argument in $arguments) { $startInfo.ArgumentList.Add($argument) }
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$process = [System.Diagnostics.Process]::new()
$process.StartInfo = $startInfo
$timer = [System.Diagnostics.Stopwatch]::StartNew()
$observed = @{}
$peakSimultaneousWorkingSet = 0L
$peakSimultaneousPrivateBytes = 0L

function Get-ProcessTree([int]$RootId) {
    $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name)
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootId)
    do {
        $added = $false
        foreach ($entry in $all) {
            if ($ids.Contains([int]$entry.ParentProcessId) -and $ids.Add([int]$entry.ProcessId)) { $added = $true }
        }
    } while ($added)
    return @($all | Where-Object { $ids.Contains([int]$_.ProcessId) })
}

try {
    [void]$process.Start()
    while (-not $process.HasExited) {
        $pollWorkingSet = 0L
        $pollPrivateBytes = 0L
        foreach ($entry in (Get-ProcessTree $process.Id)) {
            $native = Get-Process -Id $entry.ProcessId -ErrorAction SilentlyContinue
            if ($null -eq $native) { continue }
            $startedAt = try { $native.StartTime.ToUniversalTime().Ticks } catch { 0 }
            $key = ([string]$entry.ProcessId + ":" + [string]$startedAt)
            $pollWorkingSet += [long]$native.WorkingSet64
            $pollPrivateBytes += [long]$native.PrivateMemorySize64
            $observed[$key] = [ordered]@{
                pid = $entry.ProcessId
                start_time_utc_ticks = $startedAt
                name = $entry.Name
                peak_working_set_bytes = [math]::Max([long]($observed[$key].peak_working_set_bytes), [long]$native.WorkingSet64)
                peak_private_bytes = [math]::Max([long]($observed[$key].peak_private_bytes), [long]$native.PrivateMemorySize64)
                latest_cpu_ms = [math]::Round($native.TotalProcessorTime.TotalMilliseconds, 3)
            }
        }
        $peakSimultaneousWorkingSet = [math]::Max($peakSimultaneousWorkingSet, $pollWorkingSet)
        $peakSimultaneousPrivateBytes = [math]::Max($peakSimultaneousPrivateBytes, $pollPrivateBytes)
        Start-Sleep -Milliseconds 200
        $process.Refresh()
    }
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $timer.Stop()
    Start-Sleep -Milliseconds 500
    $survivingProcesses = @($observed.Values | Where-Object {
        $candidate = Get-Process -Id $_.pid -ErrorAction SilentlyContinue
        if ($null -eq $candidate) { return $false }
        $candidateStart = try { $candidate.StartTime.ToUniversalTime().Ticks } catch { -1 }
        return $candidateStart -eq $_.start_time_utc_ticks
    } | Select-Object pid, name, start_time_utc_ticks)
    [ordered]@{
        exit_code = $process.ExitCode
        wall_time_ms = $timer.ElapsedMilliseconds
        observed_process_count = $observed.Count
        peak_simultaneous_working_set_bytes = $peakSimultaneousWorkingSet
        peak_simultaneous_private_bytes = $peakSimultaneousPrivateBytes
        sum_peak_working_set_bytes = [long](($observed.Values | Measure-Object peak_working_set_bytes -Sum).Sum)
        sum_peak_private_bytes = [long](($observed.Values | Measure-Object peak_private_bytes -Sum).Sum)
        sum_latest_cpu_ms = [double](($observed.Values | Measure-Object latest_cpu_ms -Sum).Sum)
        processes = @($observed.Values)
        surviving_observed_processes_after_exit = $survivingProcesses
        stdout = $stdout
        stderr = $stderr
        limitation = "Polling every 200 ms may miss short-lived descendants and makes CPU a lower bound. Simultaneous peaks include the probe and browser tree but exclude this measurement script and CIM polling overhead. Sums of individual peaks were not necessarily simultaneous."
    } | ConvertTo-Json -Depth 5
    exit $process.ExitCode
}
finally {
    if (-not $process.HasExited) { $process.Kill($true) }
    $process.Dispose()
}
