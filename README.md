<p align="center">
  <img src="assets/ndm.png" width="128" alt="New Download Manager logo">
</p>

<h1 align="center">New Download Manager</h1>

<p align="center">
  A fast, free and open source download manager for Windows and Linux, written in Rust.<br>
  Split downloads, browser integration, video streams, a site grabber and a scheduler, in one small native app.
</p>

<p align="center">
  <a href="https://github.com/MoeJoe37/New-Download-Manager/releases"><b>Download</b></a> ·
  <a href="#features">Features</a> ·
  <a href="#custom-toolbars">Custom toolbars</a> ·
  <a href="#building-from-source">Build</a> ·
  <a href="#translating">Translate</a> ·
  <a href="LICENSE">License (AGPL-3.0)</a>
</p>

<p align="center">
  <img src="docs/main-window.png" alt="NDM main window, dark mode" width="820">
</p>

## Features

**Downloading**
- Splits every file into parts fetched over several connections at once; finished connections take over
  half of the slowest part, so they all stay busy to the end
- Resumes broken downloads, after checking that the file on the server hasn't changed
- http, https and ftp (passive and active); mms, rtmp and rtsp through ffmpeg
- Sign-in: Basic, Digest, NTLM / Negotiate (your Windows account, or your Kerberos ticket on Linux), saved site logins
- Proxies: Windows' settings, auto-configuration scripts, HTTP, HTTPS, FTP and SOCKS 4/5
- Speed limiter, download limits per period, per-server connection limits

**Browser integration**
- Extension for Chrome, Edge, Brave, Opera, Vivaldi and Firefox: downloads go to NDM, not the browser,
  and NDM starts in the notification area if it isn't running
- Right-click *Download with New Download Manager* and *Download all links*
- Download panel on video and audio players
- Keys to force or prevent a download, file-type and site exceptions
- Optional system-wide capture of plain http downloads from any program

**Video and audio streams**
- HLS (.m3u8) and DASH (.mpd), including AES-128, byte ranges, multi-period and live streams
- Video and audio joined into one MP4 or WebM without any extra program

**Organizing**
- Categories with their own folders, queues with a scheduler (start / stop times, days, retries)
- Synchronization queue that re-downloads files changed on the server
- When a queue is done: open a file, hang up, exit, shut down, sleep or hibernate

**Site Grabber**
- Explores a site from a start page and downloads the files you pick: all pictures, every PDF,
  or a whole site converted for offline browsing
- Saved projects, templates and schedules

**And**
- Dark mode and three looks: Modern, Classic and **NDM Style**, NDM's own modern design for every window
- Custom scale for the toolbar, the categories pane and the text (View → Custom scale...)
- Customizable toolbar and columns, and [custom toolbars](#custom-toolbars) with your own pictures
- Clipboard monitor, drop target, command line (`/d URL /p folder /f name /n /a /q /s`)
- Import and export of download lists, built-in help, automatic update check

<p align="center">
  <img src="docs/download-file-info.png" alt="Download File Info window" width="560"><br>
  <sub>A download handed over by the browser</sub>
</p>

<p align="center">
  <img src="docs/main-window-light.png" alt="NDM main window, light mode" width="820"><br>
  <sub>Light mode</sub>
</p>

<p align="center">
  <img src="docs/ndm-style.png" alt="NDM main window in NDM Style, dark mode" width="820"><br>
  <sub>NDM Style (View → Toolbar → NDM Style), dark mode</sub>
</p>

<p align="center">
  <img src="docs/ndm-style-light.png" alt="NDM main window in NDM Style, light mode" width="820"><br>
  <sub>NDM Style, light mode</sub>
</p>

## Installing

1. Download `NDM-Setup-<version>.exe` from [Releases](https://github.com/MoeJoe37/New-Download-Manager/releases)
   and run it. It installs for your user only (no admin rights needed).
2. Install the browser extension: open your browser's extensions page, turn on *Developer mode*, click
   *Load unpacked* and choose the `extension` folder in NDM's install folder
   (Start menu → New Download Manager → *Browser extension folder*).
3. Start NDM once, so the browser can reach it.

**Linux:** download the package for your distribution from Releases: `ndm_<version>_amd64.deb` (Debian, Ubuntu,
Mint), `ndm-<version>-1.x86_64.rpm` (Fedora, RHEL, openSUSE), `NDM-x86_64.AppImage` (any distribution: make it
executable with `chmod +x NDM-x86_64.AppImage` and run it) or `ndm-<version>-x86_64.tar.gz` (unpack and run `./ndm`). Everything the Windows version has works there too: the tray icon (on desktops with a tray: KDE, Xfce,
Cinnamon, MATE, GNOME with the AppIndicator extension), the drop target, dial-up / VPN through NetworkManager,
the desktop's proxy, PAC scripts, NTLM and Kerberos sign-in, links dropped on the main window and Firefox's cache.

NDM checks the Releases page for new versions (Help → Check for updates, and by itself every two weeks).

## Custom toolbars

A custom toolbar replaces the pictures on NDM's toolbar buttons with your own. It is a folder of PNG files, one
per button; NDM lists every such folder under **View → Toolbar** (also on the toolbar's right-click menu).

**1. Find the toolbars folder.** Choose **View → Toolbar → Open toolbars folder**. NDM creates and opens it:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\NewDownloadManager\toolbars` |
| Linux | `~/.config/NewDownloadManager/toolbars` (or `$XDG_CONFIG_HOME/NewDownloadManager/toolbars`) |

**2. Make a folder for your toolbar** inside it. The folder's name is the toolbar's name in the menu, for example
`toolbars/Sunset`.

**3. Add one PNG per button**, named after the button:

| Button | File |
|---|---|
| Add URL | `add-url.png` |
| Resume | `resume.png` |
| Stop | `stop.png` |
| Stop All | `stop-all.png` |
| Delete | `delete.png` |
| Delete Completed | `delete-completed.png` |
| Options | `options.png` |
| Scheduler | `scheduler.png` |
| Start Queue | `start-queue.png` |
| Stop Queue | `stop-queue.png` |
| Grabber | `grabber.png` |
| Tell a Friend | `tell-a-friend.png` |

Optional, for each button:

- `<button>-hot.png`: shown while the mouse is over the button (otherwise NDM lightens the normal picture)
- `<button>-disabled.png`: shown while the button can't be used (otherwise NDM greys the normal picture)
- `small/<button>.png` (and `small/<button>-hot.png`, `small/<button>-disabled.png`): drawn for
  **Small Buttons** instead of the large picture made smaller

Pictures should be square-ish with a transparent background. NDM fits each one into the button, keeping its
proportions: large buttons are 56 × 42 pixels and small ones 32 × 32 at 100% scale, so draw them at least 112 × 84
(or 96 × 96) to stay sharp at higher **View → Custom scale** sizes. A button without a picture keeps NDM's own icon,
so a toolbar can change just a few buttons.

```
toolbars/
└── Sunset/
    ├── add-url.png
    ├── add-url-hot.png
    ├── resume.png
    ├── stop.png
    ├── options.png
    └── small/
        └── add-url.png
```

**4. Choose it:** **View → Toolbar → Sunset**. **Built-in toolbar** goes back to NDM's own icons. After changing
the pictures, choose another toolbar and then yours again to reload them.

To share a toolbar, zip its folder; whoever gets it unzips it into their toolbars folder. You can post toolbars
for others in [Issues](https://github.com/MoeJoe37/New-Download-Manager/issues).

## Building from source

Requirements: Windows 10/11 x64, [Rust](https://rustup.rs) (stable, MSVC toolchain), the Windows SDK
(for the exe icon).

```sh
cargo build --release          # target\release\ndm.exe
cargo test --release           # tests
```

Installer ([Inno Setup 6](https://jrsoftware.org/isinfo.php)):

```sh
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\ndm.iss   # installer\out\NDM-Setup-1.0.1.exe
```

| Folder | What |
|---|---|
| `src/` | The app (egui UI, download engine, streams, grabber) |
| `extension/` | Browser extension (Manifest V3) |
| `help/` | Help pages, shown inside the app |
| `Languages/` | Language files |
| `installer/` | Inno Setup script |
| `tools/` | `ndm_lng.py` (language file generator), `icon.py` (app icon), `appimage.sh` (Linux AppImage) |

Linux AppImage (on Linux, or in WSL on Windows; needs `build-essential pkg-config libgtk-3-dev libxdo-dev
libayatana-appindicator3-dev` and Rust):

```sh
bash tools/appimage.sh         # installer/out/NDM-x86_64.AppImage
```

## Translating

`Languages/ndm_en.lng` lists every text NDM shows, one `English=Text` per line. It is generated from the
source by `python tools/ndm_lng.py` (`--check` tells whether it is up to date).

To add a language, copy it to `ndm_xx.lng`, change the first line (`lang=0x407 Deutsch`), and translate the
text after each `=`. Keep `%s` / `%d` and `\n`. NDM lists the file under **View → Language**.
Pull requests welcome.

## License

[GNU Affero General Public License v3.0](LICENSE). The toolbar and tree icons are drawn at run time from
Windows' own icon font (Segoe Fluent Icons); on Linux from Google's Material Icons
(`assets/linux`, Apache License 2.0).
