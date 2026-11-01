# Start-up test of ndm.exe:  powershell -File tools\smoke-windows.ps1 target\release\ndm.exe
# Passes when NDM's browser endpoint answers within 90 s without a crash.log. CI runners have no graphics
# card, so this also shows NDM starting again with wgpu (Windows' software renderer) when OpenGL can't.
param([string]$Exe)
$data = Join-Path $env:APPDATA 'NewDownloadManager'
Remove-Item -ErrorAction SilentlyContinue (Join-Path $data 'crash.log'), (Join-Path $data 'renderer')
Start-Process -FilePath $Exe | Out-Null
$ok = $false
for ($i = 0; $i -lt 90 -and -not $ok; $i++) {
    foreach ($port in 9614, 19614, 29614, 39614) {
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/config" -Method POST -Headers @{ 'X-NDM' = '1' } -UseBasicParsing -TimeoutSec 2
            if ($r.Headers['X-NDM']) { $ok = $true; "NDM answers on port $port"; break }
        } catch {}
    }
    if (-not $ok) { Start-Sleep -Seconds 1 }
}
$renderer = Join-Path $data 'renderer'
if (Test-Path $renderer) { 'renderer: ' + (Get-Content $renderer) } else { 'renderer: OpenGL' }
Get-Process ndm -ErrorAction SilentlyContinue | Stop-Process -Force
$crash = Join-Path $data 'crash.log'
if (Test-Path $crash) { '-- crash.log:'; Get-Content $crash; $ok = $false }
if ($ok) { 'NDM started' } else { 'FAIL: NDM did not start'; exit 1 }
