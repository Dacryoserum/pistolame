# Mesure RAM et CPU du spike pendant 10 s apres un temps de chauffe.
# Usage : powershell -ExecutionPolicy Bypass -File measure.ps1 [-Arguments '--rows 0'] [-Seconds 10]
# Le binaire doit etre compile : cargo build --release -p spike-slint-backend
param([string]$Arguments = '', [int]$Seconds = 10, [int]$WarmupSeconds = 5)

$exe = Join-Path $env:CARGO_TARGET_DIR 'release\spike-slint-backend.exe'
if (-not (Test-Path $exe)) { $exe = Join-Path $PSScriptRoot '..\target\release\spike-slint-backend.exe' }
$cores = [Environment]::ProcessorCount

if ($Arguments) { $p = Start-Process $exe -ArgumentList $Arguments -PassThru }
else { $p = Start-Process $exe -PassThru }
try {
    Start-Sleep -Seconds $WarmupSeconds
    $p.Refresh()
    $ramBefore = $p.WorkingSet64 / 1MB
    $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
    $peak = 0
    for ($i = 0; $i -lt $Seconds; $i++) {
        Start-Sleep -Seconds 1
        $p.Refresh()
        $peak = [math]::Max($peak, $p.WorkingSet64 / 1MB)
    }
    $cpuPct = ($p.TotalProcessorTime.TotalMilliseconds - $cpu0) / ($Seconds * 1000) / $cores * 100
    [pscustomobject]@{
        Args          = $Arguments
        'RAM (Mo)'    = [math]::Round($ramBefore, 1)
        'RAM pic (Mo)' = [math]::Round($peak, 1)
        'CPU (%)'     = [math]::Round($cpuPct, 2)
        Coeurs        = $cores
    } | Format-List
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
}
