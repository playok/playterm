# Observe a running test app. Sampling does not change its sessions or power state.
param(
    [Parameter(Mandatory)][int]$AppId,
    [Parameter(Mandatory)][string]$OutputPath,
    [ValidateRange(1,86400)][int]$Seconds = 60,
    [ValidateRange(1,60)][int]$IntervalSeconds = 1
)
$ErrorActionPreference = 'Stop'
$previous = Get-Process -Id $AppId
$previousCpu = $previous.TotalProcessorTime.TotalSeconds
$watch = [Diagnostics.Stopwatch]::StartNew()
$previousTime = 0.0
$rows = [Collections.Generic.List[object]]::new()
while ($watch.Elapsed.TotalSeconds -lt $Seconds) {
    Start-Sleep -Seconds $IntervalSeconds
    $process = Get-Process -Id $AppId -ErrorAction SilentlyContinue
    if (-not $process) { throw 'Test app exited during measurement' }
    $elapsed = $watch.Elapsed.TotalSeconds
    $cpu = $process.TotalProcessorTime.TotalSeconds
    $rows.Add([pscustomobject]@{
        elapsed_s = [Math]::Round($elapsed,3)
        working_set_bytes = $process.WorkingSet64
        private_bytes = $process.PrivateMemorySize64
        cpu_percent = [Math]::Round(100*($cpu-$previousCpu)/($elapsed-$previousTime)/[Environment]::ProcessorCount,2)
        threads = $process.Threads.Count
        handles = $process.HandleCount
        responding = $process.Responding
    })
    $previousCpu = $cpu
    $previousTime = $elapsed
    # Flush on every sample so interruption or app failure does not lose earlier observations.
    $rows | Export-Csv -LiteralPath $OutputPath -NoTypeInformation -Encoding utf8
}
$rows | Measure-Object working_set_bytes,private_bytes,cpu_percent,threads,handles -Minimum -Maximum -Average
