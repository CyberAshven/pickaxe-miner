[CmdletBinding()]
param(
    [ValidateRange(1, 60)]
    [int]$TotalMinutes = 60,

    [ValidateRange(0, 31)]
    [int]$Device = 0,

    [ValidateRange(1, 60)]
    [int]$SampleIntervalSeconds = 5,

    [string]$OutputDirectory = "artifacts\cuda-soak",

    [string]$TargetDirectory = "",

    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
$intensityCount = 5
$minimumTelemetryCoverage = 0.80
$runtimeToleranceSeconds = 1.0
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

function Get-CompetingPickaxeGpuProcesses {
    param(
        [int]$Device,
        [int[]]$ExcludeProcessId = @(),
        [string[]]$QueryLines = $null
    )

    if ($null -eq $QueryLines) {
        $QueryLines = @(
            & nvidia-smi "--id=$Device" "--query-compute-apps=pid,process_name" "--format=csv,noheader" 2>$null
        )
        if ($LASTEXITCODE -ne 0) {
            throw "Unable to query GPU $Device compute processes with nvidia-smi."
        }
    }

    $excluded = @{}
    foreach ($processId in $ExcludeProcessId) {
        $excluded[[int]$processId] = $true
    }

    $competitors = [System.Collections.Generic.List[object]]::new()
    foreach ($line in $QueryLines) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $parts = $line.Split(",", 2)
        if ($parts.Count -ne 2) {
            continue
        }
        $processId = 0
        if (![int]::TryParse($parts[0].Trim(), [ref]$processId)) {
            continue
        }
        if ($excluded.ContainsKey($processId)) {
            continue
        }
        $processName = $parts[1].Trim()
        $executableName = [System.IO.Path]::GetFileNameWithoutExtension($processName)
        if ($executableName -ieq "pickaxe_miner") {
            $competitors.Add([pscustomobject]@{
                pid = $processId
                process_name = $processName
            })
        }
    }
    return @($competitors)
}

function Get-CoverageSummary {
    param(
        [object[]]$Values,
        [int]$ExpectedSamples,
        [double]$MinimumCoverage
    )

    $observed = @($Values | Where-Object { $null -ne $_ }).Count
    $required = [Math]::Max(1, [Math]::Ceiling($ExpectedSamples * $MinimumCoverage))
    return [ordered]@{
        samples = $observed
        expected_samples = $ExpectedSamples
        required_samples = $required
        coverage_percent = if ($ExpectedSamples -gt 0) {
            [Math]::Round(100.0 * $observed / $ExpectedSamples, 3)
        } else {
            0.0
        }
        complete = ($observed -ge $required)
    }
}

function Get-SoakEvidenceCompleteness {
    param(
        [object[]]$Samples,
        [double]$ObservedRuntimeSeconds,
        [int]$ExpectedMatrixSeconds,
        [int]$SampleIntervalSeconds,
        [double]$MinimumTelemetryCoverage,
        [double]$RuntimeToleranceSeconds
    )

    $expectedSampleCount = [Math]::Max(
        1,
        [Math]::Floor($ExpectedMatrixSeconds / $SampleIntervalSeconds)
    )
    $minimumSampleCount = [Math]::Max(
        3,
        [Math]::Ceiling($expectedSampleCount * $MinimumTelemetryCoverage)
    )
    $minimumObservedSeconds = [Math]::Max(
        0.0,
        $ExpectedMatrixSeconds - $RuntimeToleranceSeconds
    )
    $durationComplete = $ObservedRuntimeSeconds -ge $minimumObservedSeconds
    $sampleCountComplete = $Samples.Count -ge $minimumSampleCount
    $workingSetCoverage = Get-CoverageSummary -Values $Samples.working_set_mib -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $privateCoverage = Get-CoverageSummary -Values $Samples.private_mib -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $cpuCoverage = Get-CoverageSummary -Values $Samples.cpu_utilization_percent -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $vramCoverage = Get-CoverageSummary -Values $Samples.gpu_vram_mib -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $gpuUtilCoverage = Get-CoverageSummary -Values $Samples.gpu_utilization_percent -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $gpuPowerCoverage = Get-CoverageSummary -Values $Samples.gpu_power_watts -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $gpuTemperatureCoverage = Get-CoverageSummary -Values $Samples.gpu_temperature_c -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $gpuGraphicsClockCoverage = Get-CoverageSummary -Values $Samples.gpu_graphics_clock_mhz -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $gpuMemoryClockCoverage = Get-CoverageSummary -Values $Samples.gpu_memory_clock_mhz -ExpectedSamples $expectedSampleCount -MinimumCoverage $MinimumTelemetryCoverage
    $telemetryComplete = (
        $sampleCountComplete -and
        $workingSetCoverage.complete -and
        $privateCoverage.complete -and
        $cpuCoverage.complete -and
        $vramCoverage.complete -and
        $gpuUtilCoverage.complete -and
        $gpuPowerCoverage.complete -and
        $gpuTemperatureCoverage.complete -and
        $gpuGraphicsClockCoverage.complete -and
        $gpuMemoryClockCoverage.complete
    )

    return [ordered]@{
        duration_complete = $durationComplete
        minimum_observed_seconds = $minimumObservedSeconds
        expected_sample_count = $expectedSampleCount
        minimum_sample_count = $minimumSampleCount
        sample_count_complete = $sampleCountComplete
        telemetry_complete = $telemetryComplete
        telemetry_coverage = [ordered]@{
            working_set = $workingSetCoverage
            private_memory = $privateCoverage
            cpu_utilization = $cpuCoverage
            gpu_vram = $vramCoverage
            gpu_utilization = $gpuUtilCoverage
            gpu_power = $gpuPowerCoverage
            gpu_temperature = $gpuTemperatureCoverage
            gpu_graphics_clock = $gpuGraphicsClockCoverage
            gpu_memory_clock = $gpuMemoryClockCoverage
        }
    }
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
            early_average_mib = $null
            late_average_mib = $null
            sustained_delta_mib = $null
            projected_trend_delta_mib = $null
            monotonic_growth = $null
            sustained_growth = $null
            growth_detected = $null
        }
    }

    $warmupIndex = [Math]::Min(
        $numeric.Count - 1,
        [Math]::Max(1, [Math]::Floor($numeric.Count * 0.1))
    )
    $steady = @($numeric[$warmupIndex..($numeric.Count - 1)])

    $monotonic = $true
    for ($i = 1; $i -lt $steady.Count; $i++) {
        if ($steady[$i] + $ToleranceMiB -lt $steady[$i - 1]) {
            $monotonic = $false
            break
        }
    }
    $delta = $steady[-1] - $steady[0]
    $monotonicGrowth = ($monotonic -and $delta -gt $ToleranceMiB)

    # A leak can still trend upward while occasionally dropping by more than the
    # per-sample tolerance. Compare stable endpoint windows and require the
    # least-squares trend to agree before calling that sustained growth.
    $windowSize = [Math]::Max(1, [Math]::Floor($steady.Count * 0.1))
    $early = @($steady[0..($windowSize - 1)])
    $lateStart = $steady.Count - $windowSize
    $late = @($steady[$lateStart..($steady.Count - 1)])
    $earlyAverage = [double](($early | Measure-Object -Average).Average)
    $lateAverage = [double](($late | Measure-Object -Average).Average)
    $sustainedDelta = $lateAverage - $earlyAverage

    $count = [double]$steady.Count
    $sumX = 0.0
    $sumY = 0.0
    $sumXY = 0.0
    $sumXX = 0.0
    for ($i = 0; $i -lt $steady.Count; $i++) {
        $x = [double]$i
        $y = [double]$steady[$i]
        $sumX += $x
        $sumY += $y
        $sumXY += $x * $y
        $sumXX += $x * $x
    }
    $denominator = ($count * $sumXX) - ($sumX * $sumX)
    $slopePerSample = if ([Math]::Abs($denominator) -gt [double]::Epsilon) {
        (($count * $sumXY) - ($sumX * $sumY)) / $denominator
    } else {
        0.0
    }
    $projectedTrendDelta = $slopePerSample * [Math]::Max(0, $steady.Count - 1)
    $sustainedGrowth = (
        $sustainedDelta -gt $ToleranceMiB -and
        $projectedTrendDelta -gt $ToleranceMiB
    )

    return [ordered]@{
        samples = $numeric.Count
        warmup_discarded_samples = $warmupIndex
        first_mib = [Math]::Round($steady[0], 3)
        final_mib = [Math]::Round($steady[-1], 3)
        peak_mib = [Math]::Round(($steady | Measure-Object -Maximum).Maximum, 3)
        delta_mib = [Math]::Round($delta, 3)
        early_average_mib = [Math]::Round($earlyAverage, 3)
        late_average_mib = [Math]::Round($lateAverage, 3)
        sustained_delta_mib = [Math]::Round($sustainedDelta, 3)
        projected_trend_delta_mib = [Math]::Round($projectedTrendDelta, 3)
        monotonic_growth = $monotonicGrowth
        sustained_growth = $sustainedGrowth
        growth_detected = ($monotonicGrowth -or $sustainedGrowth)
    }
}

if ($SelfTest) {
    $syntheticGpuProcesses = @(
        "61300, C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        "35388, D:\Qubes\pickaxe-live-latest\release\pickaxe_miner.exe"
    )
    $syntheticCompetitors = @(
        Get-CompetingPickaxeGpuProcesses -Device 0 -QueryLines $syntheticGpuProcesses
    )
    if ($syntheticCompetitors.Count -ne 1 -or $syntheticCompetitors[0].pid -ne 35388) {
        throw "Self-test failed: competing Pickaxe GPU process was not detected."
    }
    $syntheticExcluded = @(
        Get-CompetingPickaxeGpuProcesses -Device 0 -ExcludeProcessId @(35388) -QueryLines $syntheticGpuProcesses
    )
    if ($syntheticExcluded.Count -ne 0) {
        throw "Self-test failed: owned Pickaxe GPU process exclusion did not work."
    }

    $synthetic = @(
        0..9 | ForEach-Object {
            [pscustomobject]@{
                working_set_mib = 100.0
                private_mib = 200.0
                cpu_utilization_percent = if ($_ -eq 0) { $null } else { 1.0 }
                gpu_vram_mib = 600.0
                gpu_utilization_percent = 80.0
                gpu_power_watts = 100.0
                gpu_temperature_c = 70.0
                gpu_graphics_clock_mhz = 2500.0
                gpu_memory_clock_mhz = 14000.0
            }
        }
    )
    $complete = Get-SoakEvidenceCompleteness -Samples $synthetic -ObservedRuntimeSeconds 10.0 -ExpectedMatrixSeconds 10 -SampleIntervalSeconds 1 -MinimumTelemetryCoverage 0.80 -RuntimeToleranceSeconds 1.0
    if (!$complete.duration_complete -or !$complete.telemetry_complete) {
        throw "Self-test failed: complete evidence was rejected."
    }

    $early = Get-SoakEvidenceCompleteness -Samples $synthetic -ObservedRuntimeSeconds 8.9 -ExpectedMatrixSeconds 10 -SampleIntervalSeconds 1 -MinimumTelemetryCoverage 0.80 -RuntimeToleranceSeconds 1.0
    if ($early.duration_complete) {
        throw "Self-test failed: truncated runtime was accepted."
    }

    $sparse = @($synthetic[0..7] | ForEach-Object { $_.PSObject.Copy() })
    $sparse[0].gpu_power_watts = $null
    if ((Get-SoakEvidenceCompleteness -Samples $sparse -ObservedRuntimeSeconds 10.0 -ExpectedMatrixSeconds 10 -SampleIntervalSeconds 1 -MinimumTelemetryCoverage 0.80 -RuntimeToleranceSeconds 1.0).telemetry_complete) {
        throw "Self-test failed: sparse per-metric telemetry was accepted."
    }

    $missingClock = @($synthetic | ForEach-Object {
        $copy = $_.PSObject.Copy()
        $copy.gpu_graphics_clock_mhz = $null
        $copy
    })
    if ((Get-SoakEvidenceCompleteness -Samples $missingClock -ObservedRuntimeSeconds 10.0 -ExpectedMatrixSeconds 10 -SampleIntervalSeconds 1 -MinimumTelemetryCoverage 0.80 -RuntimeToleranceSeconds 1.0).telemetry_complete) {
        throw "Self-test failed: missing GPU clock telemetry was accepted."
    }

    $stableMemory = @(0..29 | ForEach-Object { 200.0 + (($_ % 3) - 1) * 4.0 })
    if ((Get-GrowthSummary -Values $stableMemory -ToleranceMiB 32).growth_detected) {
        throw "Self-test failed: bounded memory noise was classified as growth."
    }

    $sawtoothLeak = @(0..29 | ForEach-Object { 100.0 + ($_ * 4.0) })
    $sawtoothLeak[15] = 120.0
    $growth = Get-GrowthSummary -Values $sawtoothLeak -ToleranceMiB 32
    if (!$growth.growth_detected -or $growth.monotonic_growth -or !$growth.sustained_growth) {
        throw "Self-test failed: sustained sawtooth growth escaped the leak detector."
    }

    $transientSpike = @(0..29 | ForEach-Object { 220.0 })
    $transientSpike[20] = 280.0
    if ((Get-GrowthSummary -Values $transientSpike -ToleranceMiB 32).growth_detected) {
        throw "Self-test failed: one transient memory spike was classified as sustained growth."
    }

    Write-Output "cuda-soak evidence self-test: PASS"
    return
}

$preexistingPickaxe = @(Get-CompetingPickaxeGpuProcesses -Device $Device)
if ($preexistingPickaxe.Count -gt 0) {
    $details = ($preexistingPickaxe | ForEach-Object { "PID $($_.pid): $($_.process_name)" }) -join "; "
    throw "CUDA soak requires exclusive Pickaxe access to GPU $Device. Stop the competing Pickaxe process before collecting evidence: $details"
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
    $finished = Get-Date
    $observedRuntimeSeconds = ($finished - $started).TotalSeconds
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
    $expectedMatrixSeconds = $windowSeconds * $intensityCount
    $evidence = Get-SoakEvidenceCompleteness -Samples $samples -ObservedRuntimeSeconds $observedRuntimeSeconds -ExpectedMatrixSeconds $expectedMatrixSeconds -SampleIntervalSeconds $SampleIntervalSeconds -MinimumTelemetryCoverage $minimumTelemetryCoverage -RuntimeToleranceSeconds $runtimeToleranceSeconds
    $durationComplete = $evidence.duration_complete
    $telemetryComplete = $evidence.telemetry_complete
    $growthDetected = (
        $workingSetSummary.growth_detected -eq $true -or
        $privateSummary.growth_detected -eq $true -or
        $vramSummary.growth_detected -eq $true
    )
    $monotonicGrowthDetected = (
        $workingSetSummary.monotonic_growth -eq $true -or
        $privateSummary.monotonic_growth -eq $true -or
        $vramSummary.monotonic_growth -eq $true
    )
    $status = if (
        $exitCode -eq 0 -and
        !$growthDetected -and
        $durationComplete -and
        $telemetryComplete
    ) { "PASS" } else { "FAIL" }

    $summary = [ordered]@{
        status = $status
        exit_code = $exitCode
        requested_total_minutes = $TotalMinutes
        benchmark_window_seconds = $windowSeconds
        expected_matrix_seconds = $expectedMatrixSeconds
        observed_runtime_seconds = [Math]::Round($observedRuntimeSeconds, 3)
        duration_complete = $durationComplete
        minimum_observed_seconds = $evidence.minimum_observed_seconds
        sample_interval_seconds = $SampleIntervalSeconds
        sample_count = $samples.Count
        expected_sample_count = $evidence.expected_sample_count
        minimum_sample_count = $evidence.minimum_sample_count
        device = $Device
        target_directory = $TargetDirectory
        telemetry_complete = $telemetryComplete
        telemetry_coverage = $evidence.telemetry_coverage
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
        growth_detected = $growthDetected
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
    if (!$durationComplete) {
        throw "CUDA soak ended before the required benchmark duration. See $summaryPath"
    }
    if (!$telemetryComplete) {
        throw "CUDA soak telemetry coverage is incomplete. See $summaryPath"
    }
    if ($summary.growth_detected) {
        throw "Possible sustained RAM/VRAM growth detected after warm-up. See $summaryPath"
    }
} finally {
    $env:CARGO_TARGET_DIR = $previousTargetDirectory
    Pop-Location
}
