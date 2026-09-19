param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [string]$Channel = "ouaiseddy",
    [int]$Samples = 1,
    [int]$IntervalSeconds = 15,
    [int]$TimeoutSeconds = 30
)

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$stdoutPath = Join-Path $env:TEMP ("veylurk-probe-stdout-" + [guid]::NewGuid() + ".txt")
$stderrPath = Join-Path $env:TEMP ("veylurk-probe-stderr-" + [guid]::NewGuid() + ".txt")
$timer = [System.Diagnostics.Stopwatch]::StartNew()
$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $resolvedExecutable
$startInfo.ArgumentList.Add("--channel")
$startInfo.ArgumentList.Add($Channel)
$startInfo.ArgumentList.Add("--samples")
$startInfo.ArgumentList.Add([string]$Samples)
$startInfo.ArgumentList.Add("--interval")
$startInfo.ArgumentList.Add([string]$IntervalSeconds)
$startInfo.ArgumentList.Add("--timeout")
$startInfo.ArgumentList.Add([string]$TimeoutSeconds)
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$process = [System.Diagnostics.Process]::new()
$process.StartInfo = $startInfo

try {
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    $process.Refresh()
    $timer.Stop()
    [System.IO.File]::WriteAllText($stdoutPath, $stdout)
    [System.IO.File]::WriteAllText($stderrPath, $stderr)
    [ordered]@{
        exit_code = $process.ExitCode
        wall_time_ms = $timer.ElapsedMilliseconds
        cpu_time_ms = [math]::Round($process.TotalProcessorTime.TotalMilliseconds, 3)
        peak_working_set_bytes = $process.PeakWorkingSet64
        stdout = $stdoutPath
        stderr = $stderrPath
    } | ConvertTo-Json
    exit $process.ExitCode
}
finally {
    $process.Dispose()
}
