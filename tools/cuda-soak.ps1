[CmdletBinding()]
param(
    [ValidateRange(1, 60)]
    [int]$TotalMinutes = 60,

    [ValidateRange(0, 31)]
    [int]$Device = 0,

    [ValidateRange(1, 60)]
    [int]$SampleIntervalSeconds = 5,

    [string]$OutputDirectory = "artifacts\cuda-soak",

    [string]$TargetDirectory = ""
)

$ErrorActionPreference = "Stop"
$intensityCount = 5
$totalSeconds = $TotalMinutes * 60
$windowSeconds = [Math]::Floor($totalSeconds / $intensityCount)
if ($windowSeconds -lt 1) {
    throw "TotalMinutes is too short for the five required benchmark intensity windows."
}
if ($windowSeconds -gt 720) {
    throw "TotalMinutes exceeds the supported 60-minute benchmark matrix."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$outputRoot = Join-Path $repoRoot $OutputDirectory
New-Item -ItemType Directory -Force -Path $outputRoot | Out-Null

if ([string]::IsNullOrWhiteSpace($TargetDirectory)) {
    if (Test-Path -LiteralPath "D:\Qubes") {
        $TargetDirectory = "D:\Qubes\pickaxe-agent-soak"
    } else {
        $TargetDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "pickaxe-agent-soak"
    }
}
$TargetDirectory = [System.IO.Path]::GetFullPath($TargetDirectory)
New-Item -ItemType Directory -Force -Path $TargetDirectory | Out-Null

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$csvPath = Join-Path $outputRoot "cuda-soak-$stamp.csv"
$summaryPath = Join-Path $outputRoot "cuda-soak-$stamp.summary.json"
$benchmarkPath = Join-Path $outputRoot "cuda-soak-$stamp.benchmark.json"
$stderrPath = Join-Path $outputRoot "cuda-soak-$stamp.stderr.txt"

function Convert-GpuMetric {
    param([string]$Value)

    $number = 0.0
    if ([double]::TryParse(
        $Value,
        [Globalization.NumberStyles]::Float,
        [Globalization.CultureInfo]::InvariantCulture,
        [ref]$number
    )) {
        return $number
    }
    return $null
}

Push-Location $repoRoot
$previousTargetDirectory = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = $TargetDirectory
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release failed with exit code $LASTEXITCODE"
    }

    $binary = Join-Path $TargetDirectory "release\pickaxe_miner.exe"
    if (!(Test-Path -LiteralPath $binary)) {
        throw "Release binary not found: $binary"
    }

    $arguments = @(
        "benchmark",
        "--backend", "cuda",
        "--device", "$Device",
        "--seconds", "$windowSeconds",
        "--json"
    )

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $binary
    $startInfo.WorkingDirectory = $repoRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.Arguments = ($arguments -join " ")

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo
    if (!$process.Start()) {
        throw "Failed to start CUDA benchmark process."
    }

    $samples = [System.Collections.Generic.List[object]]::new()
    $started = Get-Date
    $logicalProcessorCount = [Math]::Max(1, [Environment]::ProcessorCount)
    $previousCpuSeconds = $null
    $previousCpuSample = $null

    while (!$process.HasExited) {
        $now = Get-Date
        $elapsed = ($now - $started).TotalSeconds
        $proc = Get-Process -Id $process.Id -ErrorAction SilentlyContinue

        $gpu = $null
        try {
            $gpuLine = & nvidia-smi "--id=$Device" "--query-gpu=memory.used,utilization.gpu,power.draw,temperature.gpu,clocks.gr,clocks.mem" "--format=csv,noheader,nounits" 2>$null | Select-Object -First 1
            if ($LASTEXITCODE -eq 0 -and $gpuLine) {
                $parts = $gpuLine.Split(",") | ForEach-Object { $_.Trim() }
                if ($parts.Count -eq 6) {
                    $gpu = @{
                        vram_mib = Convert-GpuMetric $parts[0]
                        utilization_percent = Convert-GpuMetric $parts[1]
                        power_watts = Convert-GpuMetric $parts[2]
                        temperature_c = Convert-GpuMetric $parts[3]
                        graphics_clock_mhz = Convert-GpuMetric $parts[4]
                        memory_clock_mhz = Convert-GpuMetric $parts[5]
                    }
                }
            }
        } catch {
            $gpu = $null
        }

        if ($proc) {
            $proc.Refresh()
            $cpuSeconds = $proc.TotalProcessorTime.TotalSeconds
            $cpuPercent = $null
            if ($null -ne $previousCpuSeconds -and $null -ne $previousCpuSample) {
                $wallSeconds = ($now - $previousCpuSample).TotalSeconds
                if ($wallSeconds -gt 0) {
                    $cpuDelta = [Math]::Max(0.0, $cpuSeconds - $previousCpuSeconds)
                    $cpuPercent = [Math]::Round(100.0 * $cpuDelta / $wallSeconds / $logicalProcessorCount, 3)
                }
            }
            $previousCpuSeconds = $cpuSeconds
            $previousCpuSample = $now

            $samples.Add([pscustomobject]@{
                timestamp = $now.ToString("o")
                elapsed_seconds = [Math]::Round($elapsed, 3)
                working_set_mib = [Math]::Round($proc.WorkingSet64 / 1MB, 3)
                private_mib = [Math]::Round($proc.PrivateMemorySize64 / 1MB, 3)
                cpu_utilization_percent = $cpuPercent
                gpu_vram_mib = if ($gpu) { $gpu.vram_mib } else { $null }
                gpu_utilization_percent = if ($gpu) { $gpu.utilization_percent } else { $null }
                gpu_power_watts = if ($gpu) { $gpu.power_watts } else { $null }
                gpu_temperature_c = if ($gpu) { $gpu.temperature_c } else { $null }
                gpu_graphics_clock_mhz = if ($gpu) { $gpu.graphics_clock_mhz } else { $null }
                gpu_memory_clock_mhz = if ($gpu) { $gpu.memory_clock_mhz } else { $null }
            })
        }

        Start-Sleep -Seconds $SampleIntervalSeconds
        $process.Refresh()
    }

    $process.WaitForExit()
    $benchmarkStdout = $process.StandardOutput.ReadToEnd()
    $benchmarkStderr = $process.StandardError.ReadToEnd()
    $benchmarkStdout | Set-Content -Encoding utf8 $benchmarkPath
    $benchmarkStderr | Set-Content -Encoding utf8 $stderrPath
    $exitCode = $process.ExitCode
    $samples | Export-Csv -NoTypeInformation -Path $csvPath

    $benchmarkReport = $null
    try {
        $benchmarkReport = $benchmarkStdout | ConvertFrom-Json
    } catch {
        throw "Benchmark did not emit valid JSON. See $benchmarkPath"
    }
    $expectedIntensities = @(10, 25, 50, 75, 100)
    $reportedIntensities = @($benchmarkReport.samples | ForEach-Object { [int]$_.intensity })
    if ($reportedIntensities.Count -ne $expectedIntensities.Count -or
        (Compare-Object $expectedIntensities $reportedIntensities)) {
        throw "Benchmark report does not contain the required 10/25/50/75/100 intensity matrix."
    }

    function Get-GrowthSummary {
        param(
            [object[]]$Values,
            [double]$ToleranceMiB
        )

        $numeric = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
        if ($numeric.Count -lt 3) {
            return [ordered]@{
                samples = $numeric.Count
                first_mib = $null
                final_mib = $null
                peak_mib = $null
                delta_mib = $null
                monotonic_growth = $null
            }
        }

        $warmupIndex = [Math]::Min($numeric.Count - 1, [Math]::Max(1, [Math]::Floor($numeric.Count * 0.1)))
        $steady = @($numeric[$warmupIndex..($numeric.Count - 1)])
        $monotonic = $true
        for ($i = 1; $i -lt $steady.Count; $i++) {
            if ($steady[$i] + $ToleranceMiB -lt $steady[$i - 1]) {
                $monotonic = $false
                break
            }
        }
        $delta = $steady[-1] - $steady[0]

        return [ordered]@{
            samples = $numeric.Count
            warmup_discarded_samples = $warmupIndex
            first_mib = [Math]::Round($steady[0], 3)
            final_mib = [Math]::Round($steady[-1], 3)
            peak_mib = [Math]::Round(($steady | Measure-Object -Maximum).Maximum, 3)
            delta_mib = [Math]::Round($delta, 3)
            monotonic_growth = ($monotonic -and $delta -gt $ToleranceMiB)
        }
    }

    function Get-MetricSummary {
        param([object[]]$Values)

        $numeric = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
        if ($numeric.Count -eq 0) {
            return [ordered]@{ samples = 0; min = $null; average = $null; max = $null }
        }
        $measure = $numeric | Measure-Object -Minimum -Maximum -Average
        return [ordered]@{
            samples = $numeric.Count
            min = [Math]::Round([double]$measure.Minimum, 3)
            average = [Math]::Round([double]$measure.Average, 3)
            max = [Math]::Round([double]$measure.Maximum, 3)
        }
    }

    $workingSetSummary = Get-GrowthSummary -Values $samples.working_set_mib -ToleranceMiB 32
    $privateSummary = Get-GrowthSummary -Values $samples.private_mib -ToleranceMiB 32
    $vramSummary = Get-GrowthSummary -Values $samples.gpu_vram_mib -ToleranceMiB 32
    $monotonicGrowthDetected = (
        $workingSetSummary.monotonic_growth -eq $true -or
        $privateSummary.monotonic_growth -eq $true -or
        $vramSummary.monotonic_growth -eq $true
    )
    $status = if ($exitCode -eq 0 -and !$monotonicGrowthDetected) { "PASS" } else { "FAIL" }

    $summary = [ordered]@{
        status = $status
        exit_code = $exitCode
        requested_total_minutes = $TotalMinutes
        benchmark_window_seconds = $windowSeconds
        expected_matrix_seconds = $windowSeconds * $intensityCount
        sample_interval_seconds = $SampleIntervalSeconds
        sample_count = $samples.Count
        device = $Device
        target_directory = $TargetDirectory
        working_set = $workingSetSummary
        private_memory = $privateSummary
        gpu_vram = $vramSummary
        cpu_utilization_percent = Get-MetricSummary -Values $samples.cpu_utilization_percent
        gpu_utilization_percent = Get-MetricSummary -Values $samples.gpu_utilization_percent
        gpu_power_watts = Get-MetricSummary -Values $samples.gpu_power_watts
        gpu_temperature_c = Get-MetricSummary -Values $samples.gpu_temperature_c
        gpu_graphics_clock_mhz = Get-MetricSummary -Values $samples.gpu_graphics_clock_mhz
        gpu_memory_clock_mhz = Get-MetricSummary -Values $samples.gpu_memory_clock_mhz
        throughput = @($benchmarkReport.samples | ForEach-Object {
            [ordered]@{
                intensity = [int]$_.intensity
                elapsed_seconds = [double]$_.elapsed_seconds
                candidates = [uint64]$_.candidates
                batches = [uint64]$_.batches
                candidates_per_second = [double]$_.candidates_per_second
                candidates_per_watt = $_.candidates_per_watt
            }
        })
        monotonic_growth_detected = $monotonicGrowthDetected
        benchmark_json = $benchmarkPath
        samples_csv = $csvPath
        stderr = $stderrPath
    }

    $summary | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $summaryPath
    Get-Content $summaryPath

    if ($exitCode -ne 0) {
        throw "CUDA benchmark exited with code $exitCode. See $stderrPath"
    }
    if ($summary.monotonic_growth_detected) {
        throw "Possible monotonic RAM/VRAM growth detected after warm-up. See $summaryPath"
    }
} finally {
    $env:CARGO_TARGET_DIR = $previousTargetDirectory
    Pop-Location
}
