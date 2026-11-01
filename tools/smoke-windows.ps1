# Start-up test of ndm.exe:  powershell -File tools\smoke-windows.ps1 target\release\ndm.exe
# Passes when NDM's browser endpoint answers within 90 s without a crash.log. CI runners have no graphics
# card, so this also shows NDM starting again with wgpu (Windows' software renderer) when OpenGL can't.
param([string]$Exe)
$data = Join-Path $env:APPDATA 'NewDownloadManager'
$out = Join-Path $env:TEMP 'ndm-smoke'
New-Item -ItemType Directory -Force $out | Out-Null
Remove-Item -ErrorAction SilentlyContinue (Join-Path $data 'crash.log'), (Join-Path $data 'renderer'), (Join-Path $data 'renderer-error.log')
Start-Process -FilePath $Exe -RedirectStandardOutput "$out\out.txt" -RedirectStandardError "$out\err.txt" | Out-Null
$ok = $false
for ($i = 0; $i -lt 90 -and -not $ok; $i++) {
    # only ask a port something listens on (a refused connection takes Windows two seconds)
    $ports = @(Get-NetTCPConnection -State Listen -LocalAddress 127.0.0.1 -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -in 9614, 19614, 29614, 39614 } | ForEach-Object LocalPort)
    foreach ($port in $ports) {
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/config" -Method POST -Headers @{ 'X-NDM' = '1' } -Body '' -UseBasicParsing -TimeoutSec 5
            if ($r.Headers['X-NDM']) { $ok = $true; "NDM answers on port $port"; break }
        } catch { "port $port`: $_" }
    }
    if (-not $ok) { Start-Sleep -Seconds 1 }
}
$renderer = Join-Path $data 'renderer'
if (Test-Path $renderer) { 'renderer: ' + (Get-Content $renderer) } else { 'renderer: OpenGL' }
if (-not $ok) {
    '-- NDM windows:'
    Get-Process ndm -ErrorAction SilentlyContinue | ForEach-Object { "pid $($_.Id): '$($_.MainWindowTitle)'" }
    $err = Join-Path $data 'renderer-error.log'
    if (Test-Path $err) { '-- renderer-error.log:'; Get-Content $err }
    '-- output:'; Get-Content "$out\out.txt", "$out\err.txt" -ErrorAction SilentlyContinue
}
Get-Process ndm -ErrorAction SilentlyContinue | Stop-Process -Force
$crash = Join-Path $data 'crash.log'
if (Test-Path $crash) { '-- crash.log:'; Get-Content $crash; $ok = $false }
if ($ok) { 'NDM started' } else { 'FAIL: NDM did not start'; exit 1 }
