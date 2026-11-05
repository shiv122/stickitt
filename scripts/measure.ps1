# Measures Stick-itt's footprint with N notes open (Windows).
# Usage: pwsh scripts/measure.ps1 [-Counts 1,10,30] [-IdleSeconds 10] [-Big]
# -Big opens long, richly formatted notes (about 360 lines each) in larger windows.
param([int[]]$Counts = @(1, 10, 30), [int]$IdleSeconds = 10, [switch]$Big)

$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\release\stickit.exe"
if (-not (Test-Path $exe)) {
    Push-Location $root; cargo build --release; Pop-Location
}

$rows = foreach ($n in $Counts) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $argv = @("--bench", $n); if ($Big) { $argv += "--big" }
    $p = Start-Process $exe -ArgumentList $argv -PassThru
    while ($sw.ElapsedMilliseconds -lt 10000) {
        $p.Refresh()
        if ($p.MainWindowHandle -ne 0) { break }
        Start-Sleep -Milliseconds 5
    }
    $firstWindow = $sw.ElapsedMilliseconds

    Start-Sleep -Seconds 3   # let every window paint and settle
    $p.Refresh()
    $cpu0 = $p.TotalProcessorTime
    Start-Sleep -Seconds $IdleSeconds
    $p.Refresh()
    $cpuMs = ($p.TotalProcessorTime - $cpu0).TotalMilliseconds

    [pscustomobject]@{
        Notes            = $n
        'FirstWindow ms' = $firstWindow
        'WorkingSet MB'  = [math]::Round($p.WorkingSet64 / 1MB, 1)
        'Private MB'     = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        'Idle CPU %'     = [math]::Round($cpuMs / ($IdleSeconds * 10), 3)  # % of one core
        Threads          = $p.Threads.Count
    }
    Stop-Process -Id $p.Id
    Start-Sleep -Milliseconds 500
}
$rows | Format-Table -AutoSize
