# Browser downloads show only their own windows:  powershell -File tools\tray-windows.ps1 target\release\ndm.exe
# NDM in the notification area (started for the browser, or its window closed) or minimized gets a download
# the way the browser extension sends one (POST /add). Its File Info / progress / "Download complete" windows
# must show; the main window must stay where it was (hidden, minimized, or behind another program). Fails otherwise.
param([string]$Exe)
$ErrorActionPreference = 'Stop'
Add-Type @"
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Text;
public static class W {
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  // whether `top` is above `other` in the z-order (GW_HWNDPREV walks up)
  public static bool Above(IntPtr top, IntPtr other) { for (var h = GetWindow(other, 3); h != IntPtr.Zero; h = GetWindow(h, 3)) if (h == top) return true; return false; }
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool Proc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(Proc f, IntPtr l);
  public static List<IntPtr> Of(int pid) {
    var r = new List<IntPtr>();
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid) r.Add(h); return true; }, IntPtr.Zero);
    return r;
  }
  public static string Title(IntPtr h) { var s = new StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
}
"@
$MainTitle = 'New Download Manager 1.0'
$data = Join-Path $env:APPDATA 'NewDownloadManager'
$tmp = Join-Path $env:TEMP 'ndm-tray-test'
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$tmp\site", "$tmp\dl" | Out-Null
$bytes = New-Object byte[] (8MB); (New-Object Random 7).NextBytes($bytes)
[IO.File]::WriteAllBytes("$tmp\site\file.bin", $bytes)
$server = Start-Process python -ArgumentList '-m', 'http.server', '8765', '--bind', '127.0.0.1' -WorkingDirectory "$tmp\site" -PassThru -WindowStyle Hidden

function Port {
    for ($i = 0; $i -lt 90; $i++) {
        $ports = @(Get-NetTCPConnection -State Listen -LocalAddress 127.0.0.1 -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -in 9614, 19614, 29614, 39614 } | ForEach-Object LocalPort)
        foreach ($p in $ports) {
            try {
                $r = Invoke-WebRequest -Uri "http://127.0.0.1:$p/config" -Method POST -Headers @{ 'X-NDM' = '1' } -Body '' -UseBasicParsing -TimeoutSec 5
                if ($r.Headers['X-NDM']) { return $p }
            } catch {}
        }
        Start-Sleep -Seconds 1
    }
    throw 'NDM did not start'
}
# every ndm process: NDM starts itself again with another renderer when OpenGL fails (CI has no GPU)
function All { @(Get-Process ndm -ErrorAction SilentlyContinue | ForEach-Object { [W]::Of($_.Id) } | ForEach-Object { $_ }) }
function Shown { @(All | Where-Object { [W]::IsWindowVisible($_) -and [W]::Title($_) }) }
function MainOf {
    for ($i = 0; $i -lt 20; $i++) {
        $m = @(All | Where-Object { [W]::Title($_) -eq $MainTitle })
        if ($m.Count -gt 0) { return $m[0] }
        Start-Sleep -Milliseconds 500
    }
    $null
}

$failed = @()
foreach ($case in 'tray', 'closed', 'minimized', 'behind') {
    "== $case"
    Get-Process ndm -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 1
    New-Item -ItemType Directory -Force $data | Out-Null
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    # no tip of the day or update check at start: only the download's windows may come up
    $json = @{ settings = @{ tips_startup = $false; update_checked = $now; base_dir = "$tmp\dl"; show_progress = $true; show_complete = $true; start_at_once = $true }; items = @(); next_id = 1 } | ConvertTo-Json -Depth 5
    # no byte order mark: NDM's JSON reader would skip the file
    [IO.File]::WriteAllText((Join-Path $data 'state.json'), $json, (New-Object Text.UTF8Encoding $false))
    if ($case -eq 'tray') { Start-Process -FilePath $Exe -ArgumentList '/tray' | Out-Null } else { Start-Process -FilePath $Exe | Out-Null }
    $port = Port
    Start-Sleep -Seconds 3
    $main = MainOf
    if ($null -eq $main) { "no main window: " + ((All | ForEach-Object { "'$([W]::Title($_))' visible=$([W]::IsWindowVisible($_))" }) -join ', '); $failed += "$case (no main window)"; continue }
    if ($case -eq 'closed') { [W]::PostMessage($main, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null; Start-Sleep -Seconds 2 }
    if ($case -eq 'minimized') { [W]::ShowWindow($main, 6) | Out-Null; Start-Sleep -Seconds 2 }
    # open, but behind another program (the browser): it must stay behind it
    $other = $null
    if ($case -eq 'behind') {
        $np = Start-Process notepad -PassThru; Start-Sleep -Seconds 2
        $other = (Get-Process -Id $np.Id).MainWindowHandle
        if ($other -eq [IntPtr]::Zero) { $other = (Get-Process notepad | Where-Object MainWindowHandle -ne 0 | Select-Object -First 1).MainWindowHandle }
        [W]::SetForegroundWindow($other) | Out-Null; Start-Sleep -Seconds 1
    }
    $before = switch ($case) { 'minimized' { [W]::IsIconic($main) } 'behind' { [W]::IsWindowVisible($main) -and -not [W]::Above($main, $other) } default { -not [W]::IsWindowVisible($main) } }
    "main window before: visible=$([W]::IsWindowVisible($main)) minimized=$([W]::IsIconic($main))"
    if (-not $before) { $failed += "$case (main window not in the tray / minimized before the download)"; continue }
    $seen = @{}; $opened = $false
    # a download as the browser hands it over (File Info), then one that starts at once (/n: progress,
    # then "Download complete")
    foreach ($silent in $false, $true) {
        $body = @{ url = 'http://127.0.0.1:8765/file.bin'; cookies = ''; proxy = ''; filename = "$case-$silent.bin"; referrer = 'http://127.0.0.1:8765/'; silent = $silent } | ConvertTo-Json
        Invoke-WebRequest -Uri "http://127.0.0.1:$port/add" -Method POST -Headers @{ 'X-NDM' = '1'; 'Content-Type' = 'application/json' } -Body $body -UseBasicParsing -TimeoutSec 10 | Out-Null
        for ($i = 0; $i -lt 30; $i++) {
            Start-Sleep -Milliseconds 500
            foreach ($h in Shown) {
                $t = [W]::Title($h)
                if (-not $seen[$t]) { $seen[$t] = $true; "  window: $t" }
            }
            $out = switch ($case) { 'minimized' { -not [W]::IsIconic($main) } 'behind' { [W]::Above($main, $other) } default { [W]::IsWindowVisible($main) } }
            if ($out -and -not $opened) { $opened = $true; "  main window came out ($(if ($silent) { 'started download' } else { 'File Info' }))" }
        }
    }
    if ($other) { Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force }
    $popups = @($seen.Keys | Where-Object { $_ -ne $MainTitle })
    if ($opened) { $failed += "$case (the main window opened)" }
    if (-not $popups) { $failed += "$case (no download window showed)" }
    if (-not ($seen.Keys | Where-Object { $_ -like '*Download complete*' })) { $failed += "$case (no Download complete window)" }
}
Get-Process ndm -ErrorAction SilentlyContinue | Stop-Process -Force
Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
$crash = Join-Path $data 'crash.log'
if (Test-Path $crash) { '-- crash.log:'; Get-Content $crash; $failed += 'crash' }
if ($failed) { 'FAIL: ' + ($failed -join '; '); exit 1 }
'Browser downloads keep the main window where it was'
