# Mesure les trois maquettes : temps d'ouverture de la fenêtre, RAM et CPU pendant la lecture.
# Usage : powershell -ExecutionPolicy Bypass -File mesurer.ps1   (après cargo build --release)
param([string[]]$targets = @('proto-slint', 'proto-iced', 'proto-gpui', 'proto-slint-am', 'proto-iced-am'))

$ErrorActionPreference = 'Stop'
$dir = Join-Path $PSScriptRoot 'target\release'
$cores = [Environment]::ProcessorCount
$results = @()

foreach ($name in $targets) {
    $exe = Join-Path $dir "$name.exe"
    if (-not (Test-Path $exe)) { Write-Warning "$exe introuvable"; continue }

    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process $exe -PassThru
    while ($p.MainWindowHandle -eq 0 -and $sw.ElapsedMilliseconds -lt 15000) {
        Start-Sleep -Milliseconds 5
        $p.Refresh()
    }
    $openMs = $sw.ElapsedMilliseconds

    # Laisse l'animation tourner, puis mesure le CPU sur 5 s.
    Start-Sleep -Seconds 3
    $p.Refresh()
    $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Seconds 5
    $p.Refresh()
    $cpuPct = ($p.TotalProcessorTime.TotalMilliseconds - $cpu0) / 5000 / $cores * 100

    $results += [pscustomobject]@{
        Maquette        = $name
        'Fenêtre (ms)'  = $openMs
        'RAM (Mo)'      = [math]::Round($p.WorkingSet64 / 1MB, 1)
        'Privée (Mo)'   = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        'CPU (%)'       = [math]::Round($cpuPct, 1)
        'Exe (Mo)'      = [math]::Round((Get-Item $exe).Length / 1MB, 1)
    }
    Stop-Process -Id $p.Id -Force
    Start-Sleep -Seconds 1
}

$results | Format-Table -AutoSize
