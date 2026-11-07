<p align="center">
  <img src="assets/ndm.png" width="128" alt="New Download Manager logo">
</p>

<h1 align="center">New Download Manager</h1>

<p align="center">
  A fast, free and open source download manager for Windows, macOS and Linux, written in Rust.<br>
  Split downloads, browser integration, video streams, a site grabber and a scheduler, in one small native app.
</p>

<p align="center">
  <a href="https://github.com/MoeJoe37/New-Download-Manager/releases"><b>Download</b></a> ·
  <a href="#features">Features</a> ·
  <a href="#videos-and-audio-with-yt-dlp">yt-dlp</a> ·
  <a href="#custom-toolbars">Custom toolbars</a> ·
  <a href="#custom-category-icons">Category icons</a> ·
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
- Opens a new connection every quarter second until the download runs at full speed, keeps connections
  open between parts and remembers the server's address, so parts start without a new handshake
- Notices a server that allows fewer connections (busy / too many requests) and keeps to what it allows
  instead of failing; can spread the connections over several networks at once (Wi-Fi, Ethernet, USB tethering)
- Resumes broken downloads, after checking that the file on the server hasn't changed
- http, https and ftp (passive and active); mms, rtmp and rtsp through ffmpeg
- Sign-in: Basic, Digest, NTLM / Negotiate (your Windows account, or your Kerberos ticket on Linux), saved site logins
- Proxies: Windows' settings, auto-configuration scripts, HTTP, HTTPS, FTP and SOCKS 4/5
- Speed limiter (also only between set hours), download limits per period, per-server connection limits
- Failed downloads in a queue are tried again after a growing wait (15 s, 30 s, 1 min... up to 10 min)
- File Properties → **Checksum...**: the file's SHA-256, SHA-1 and MD5, checked against one you paste

**Browser integration**
- Extension for Chrome, Edge, Brave, Opera, Vivaldi and Firefox: downloads go to NDM, not the browser,
  and NDM starts in the notification area if it isn't running
- Right-click *Download with New Download Manager* and *Download all links*
- Download panel on video and audio players: no need to play the video first, and right-click it to hide
  it on a site or turn it off (the extension has its own options page)
- After a download, the processing (joining video and audio or a stream's pieces, moving the file) shows in the
  same progress bar
- Files a page makes itself (Telegram Web's videos and music come from its service worker) are fetched
  inside the page and handed to NDM piece by piece, so they arrive whole
- Keys to force or prevent a download, file-type and site exceptions
- Optional system-wide capture of plain http downloads from any program

**Video and audio streams**
- HLS (.m3u8) and DASH (.mpd), including AES-128, byte ranges, multi-period and live streams
- Video and audio joined into one MP4 or WebM without any extra program
- Video and audio websites through [yt-dlp](https://github.com/yt-dlp/yt-dlp), including YouTube,
  Vimeo, TikTok, Instagram, X, Reddit, Twitch and SoundCloud. Any website supported by the installed
  yt-dlp can be submitted; NDM's list of familiar sites is only a shortcut for automatic detection
- Direct video and audio streams use NDM's parallel connections, adaptive file segmentation and
  connection reuse. Video and audio download together; HLS/DASH streams handled by yt-dlp use concurrent
  fragments, with the connection count from NDM's settings
- **Downloads → Set yt-dlp** installs and updates yt-dlp and Deno on Windows, macOS and Linux.
  [Setup and usage](#videos-and-audio-with-yt-dlp)

**Organizing**
- Categories with their own folders, queues with a scheduler (start / stop times, days, retries)
- Synchronization queue that re-downloads files changed on the server
- When a queue is done: open a file, hang up, exit, shut down, sleep or hibernate

**Site Grabber**
- Explores a site from a start page and downloads the files you pick: all pictures, every PDF,
  or a whole site converted for offline browsing
- Saved projects, templates and schedules

**And**
- Dark mode and two looks: Modern and **NDM Style**, NDM's own modern design for every window
- Custom scale for the toolbar, the categories pane and the text, including dropdown menus and their contents (View → Custom scale...)
- Customizable toolbar and columns, [custom toolbars](#custom-toolbars) and [custom category icons](#custom-category-icons) with your own pictures
- Filter box above the list, clipboard monitor, drop target, command line (`/d URL /p folder /f name /n /a /q /s`)
- Import and export of download lists, built-in help, automatic update check
- English, Spanish, French and Arabic (right to left: every window is mirrored), and any language you add
  (View → Language, see [Translating](#translating))
- Starts on any machine: when the graphics driver's OpenGL doesn't work (virtual machines, remote desktops,
  old drivers) NDM starts again with Direct3D / Vulkan / Metal or Windows' software renderer, and when another
  program holds its browser port it takes the next one (the extension finds it)

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

Every file is on the [Releases](https://github.com/MoeJoe37/New-Download-Manager/releases) page:

| System | File |
|---|---|
| Windows 10/11, x64 | `NDM-Setup-<version>.exe`, or `NDM-<version>-windows-x64.zip` (portable) |
| Windows 11 on ARM (Snapdragon) | `NDM-ARM64-Setup-<version>.exe`, or `NDM-<version>-windows-arm64.zip` |
| macOS 11 or later (Apple Silicon and Intel) | `NDM-<version>-macOS.dmg` |
| Linux x86_64 | `ndm_<version>_amd64.deb`, `ndm-<version>-1.x86_64.rpm`, `NDM-x86_64.AppImage`, `ndm-<version>-x86_64.tar.gz` |
| Linux aarch64 (Raspberry Pi 4/5, ARM laptops and servers) | `ndm_<version>_arm64.deb`, `ndm-<version>-1.aarch64.rpm`, `NDM-aarch64.AppImage`, `ndm-<version>-aarch64.tar.gz` |

**Windows:**

1. Download the setup for your processor and run it. It installs for your user only (no admin rights
   needed). The portable zip runs from any folder (a USB stick, too): unpack it and start `ndm.exe`.
2. Install the browser extension: open your browser's extensions page, turn on *Developer mode*, click
   *Load unpacked* and choose the `extension` folder in NDM's install folder
   (Start menu → New Download Manager → *Browser extension folder*). Or choose **Tasks → Export
   extension...** in NDM, choose **Chromium-based browsers** or **Firefox / Mozilla-based browsers**,
   and pick a destination. NDM opens the matching folder with an `INSTALL.txt` guide. Chromium browsers
   use **Load unpacked**. For Firefox, Zen, Waterfox and other Firefox browsers, open `about:addons`,
   use the gear menu → **Install Add-on From File...**, select `NDM-Firefox.xpi` and confirm installation.
   The Firefox add-on is signed by Mozilla and stays installed after browser restarts.
3. Start NDM once, so the browser can reach it.

**macOS:** open the `.dmg` and drag *NDM* onto *Applications*. NDM isn't notarized by Apple, so the first
time, right-click (or Control-click) NDM in Applications and choose **Open**, then **Open** again; after that
it starts normally. (Or run `xattr -dr com.apple.quarantine /Applications/NDM.app` once in Terminal.) Its
icon sits in the menu bar while it runs; links can be dropped on its window or on the drop target. Load the Chromium browser extension from
`~/Library/Application Support/NewDownloadManager/extension`, the copy NDM keeps there so updates don't move
it (*Options → General → Reintegrate All* shows the steps for each browser). For Firefox, use the signed export described above.

**Linux:** download the package for your distribution and processor: `.deb` (Debian, Ubuntu, Mint, Raspberry
Pi OS), `.rpm` (Fedora, RHEL, openSUSE), the AppImage (any distribution: make it executable with
`chmod +x NDM-x86_64.AppImage` and run it) or the `.tar.gz` (unpack and run `./ndm`). Everything the Windows version has works there too: the tray icon (on desktops with a tray: KDE, Xfce,
Cinnamon, MATE, GNOME with the AppIndicator extension), the drop target, dial-up / VPN through NetworkManager,
the desktop's proxy, PAC scripts, NTLM and Kerberos sign-in, links dropped on the main window and Firefox's cache.

NDM checks the Releases page for new versions (Help → Check for updates, and by itself every two weeks).

## Videos and audio with yt-dlp

1. Choose **Downloads → Set yt-dlp**. NDM downloads the correct yt-dlp and Deno builds for your system,
   verifies their SHA-256 checksums and keeps its copies up to date. You can also put your own `yt-dlp`
   (`yt-dlp.exe` on Windows) beside NDM or on PATH.
2. Paste the video's own page address into **Add URL**, click the extension's **Download this video**
   panel, or right-click the page → **Download this video with New Download Manager**. This page menu
   is available on every HTTP/HTTPS site, including sites outside NDM's automatic detection list.
   On a feed, open the individual video's page first. Audio-only pages work too. The single-video
   action takes the first item from a playlist-only page.
3. Choose the file's folder and start. NDM uses its segmented engine for ordinary HTTP/HTTPS media,
   fetching video and audio at the same time. When a stream needs yt-dlp's own downloader, it keeps
   the extracted format's URLs, headers and cookies and downloads HLS/DASH fragments concurrently.
   A stream rejected by NDM's engine falls back to yt-dlp without restarting its companion stream.

The **maximum connections** setting controls NDM's connections and yt-dlp's fragment concurrency
(up to 64 concurrent fragments per stream). Servers can impose connection or speed limits; live and
protocol-specific streams may need a sequential downloader. Faster downloading depends on the server
and your connection.

NDM can join fragmented MP4/M4A and WebM streams itself. For the widest container and protocol support,
install [ffmpeg](https://ffmpeg.org/download.html) beside NDM or on PATH. If joining fails, NDM keeps the
separate streams and reports the error instead of marking a silent video as complete.

Site compatibility follows the installed [yt-dlp supported sites](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md)
and its generic extractor. NDM does not restrict explicit page requests to a fixed site list. Private,
age-restricted or regional content may need a signed-in browser session or other site requirements;
DRM-protected and unavailable media remain subject to yt-dlp's limitations. Updating yt-dlp is often
necessary when a site changes.

**Tip:** use the video's page address, rather than a temporary CDN link or one stream fragment. NDM
lets yt-dlp find the complete media and then uses the appropriate parallel download path. Browser
captures show their File Info/progress and Download complete popups while the main window stays in
the notification area; open it yourself from the tray when you need the download list.

### Download selected videos from a playlist

Choose **Downloads → Download video playlist**, enter the playlist address and choose **List videos**.
Select the videos you want and choose **Queue selected videos**. NDM lists up to the first 200 playlist
positions and resolves each queued video separately when its download starts. Each output name includes
its NDM job ID so videos with the same title can run concurrently.

Public access is the default. The browser picker lists only supported browsers whose installed
executables NDM detects when the dialog opens. For a playlist or video that requires sign-in, choose
one of those browsers. Zen, Waterfox, LibreWolf and Floorp use their own Firefox-compatible profiles;
Chrome, Edge, Chromium, Brave, Vivaldi, Opera and Whale use yt-dlp's corresponding cookie source.
Safari is offered only on macOS. yt-dlp loads the cookies separately for each lookup and queued video,
with their domain and path scopes. Browser profile access depends on the browser and operating system. Some browsers must be
closed before yt-dlp can read their profile.

### Checksum verification and diagnostic reports

For a completed download, open **File Properties → Verify checksum**. Paste a checksum, choose a local
SHA-256 file, or enter the publisher’s HTTPS checksum-list address and click **Fetch checksum**.
Publisher lists must name the downloaded file exactly. Review the matched filename and hash, then
choose **Verify matched hash**. SHA-1 and MD5 are available for pasted values.

**Help → Report a problem** opens an editable local preview. It includes version and platform details
and, when available, an NDM crash module and line number. Review and copy the report, then paste it
into the issue form. Raw panic messages and stack traces stay in the local logs.

### HTTP connection and macOS capture options

The first direct HTTPS request negotiates the protocol with the server. Once a download uses
additional connections for file ranges or parallel media fragments, those requests can use HTTP/1.1
to get separate TCP connections. This can help when the server limits speed per connection. If a
server rejects that protocol and the retry succeeds over HTTP/2, NDM remembers the result for that
server during the session. **Options → Connection → HTTP/1.1...** can force HTTP/1.1 for exact hosts.
The local comparison fixture runs with `cargo test --release http_protocol_benchmark -- --ignored --nocapture`.

On macOS, **Options → General → macOS HTTP capture** can enable plain HTTP capture for one selected
network service until NDM quits. macOS may ask for administrator authorization. The browser extension
continues to handle HTTPS. NDM saves the service’s previous web proxy values before applying the change,
restores them on exit or at the next startup after a crash, and offers **Restore web proxy** in the
same dialog. Existing web proxies, automatic proxy settings and SOCKS on the selected service block
activation. Other proxy fields are preserved.

## Browser media formats and download recovery

Failed downloads offer **Recover download** in their context menu and **Recover...** in the progress window. The recovery dialog offers actions for expired links, credentials, proxy settings, missing media tools and storage errors. Restarting a changed file is an explicit choice.

NDM serializes concurrent saves and keeps the previous valid download list in `state.json.bak`. It recovers that backup if the main list cannot be read, preserves unreadable files with a `.bad` suffix, and reports save failures in the app.

The browser panel follows the current player as its source changes. When the desktop app can read
a manifest, the panel lists detected HLS variants or single-period DASH representations with their
known resolution, codecs and audio. Use the existing **Options → General → Customize** video panel
settings to hide individual formats, including TS. This changes the menu without changing the
download engine's format support.

Compatible HLS transport streams can offer TS and MP4 separately. For a standalone TS source,
NDM checks a limited sample before offering MP4 or MKV. The panel marks MP4 conversion when the
inspected codecs require it. FFmpeg and ffprobe validate the selected output before NDM publishes
it. If a separate video and audio merge fails, **Resume** can use the completed tracks.

Direct HTTPS file and media transfers negotiate HTTP/2 with servers that offer it. Existing proxy
and connection-based authentication routes use HTTP/1.1. When a signed address returns 403, reopen
its source page and use **Refresh download address** to capture a fresh authorized link. NDM checks
the resource before using any saved partial bytes.

[Full changes and verification](docs/v-codex-2.8.0.md)

## Custom toolbars

A custom toolbar replaces the pictures on NDM's toolbar buttons with your own. It is a folder of PNG files, one
per button; NDM lists every such folder under **View → Toolbar** (also on the toolbar's right-click menu).

**1. Find the toolbars folder.** Choose **View → Toolbar → Open toolbars folder**. NDM creates and opens it:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\NewDownloadManager\toolbars` |
| Linux | `~/.config/NewDownloadManager/toolbars` (or `$XDG_CONFIG_HOME/NewDownloadManager/toolbars`) |
| macOS | `~/Library/Application Support/NewDownloadManager/toolbars` |

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

Pictures should be square-ish with a transparent background. NDM fits each one into the button, keeping its
proportions: the picture area is 56 × 42 pixels at 100% scale, so draw them at least 112 × 84
to stay sharp at higher **View → Custom scale** sizes. A button without a picture keeps NDM's own icon,
so a toolbar can change just a few buttons. Older sets with pictures only in a `small/` folder still work as a fallback.

```
toolbars/
└── Sunset/
    ├── add-url.png
    ├── add-url-hot.png
    ├── resume.png
    ├── stop.png
    └── options.png
```

**4. Choose it:** **View → Toolbar → Sunset**. **Built-in toolbar** goes back to NDM's own icons. After changing
the pictures, choose another toolbar and then yours again to reload them.

To share a toolbar, zip its folder; whoever gets it unzips it into their toolbars folder. You can post toolbars
for others in [Issues](https://github.com/MoeJoe37/New-Download-Manager/issues).

**Help → Look for new toolbars** opens this guide.

## Custom category icons

A category icon set replaces the pictures in the categories pane (All Downloads, Unfinished, Finished, the
categories, Grabber projects and Queues) with your own. Like a toolbar, it is a folder of PNG files; NDM lists
every such folder under **View → Category icons**. It works in both styles (Modern and NDM Style).

**1. Find the category icons folder.** Choose **View → Category icons → Open category icons folder**. NDM creates
and opens it:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\NewDownloadManager\category-icons` |
| Linux | `~/.config/NewDownloadManager/category-icons` (or `$XDG_CONFIG_HOME/NewDownloadManager/category-icons`) |
| macOS | `~/Library/Application Support/NewDownloadManager/category-icons` |

**2. Make a folder for your set** inside it. The folder's name is the set's name in the menu, for example
`category-icons/Candy`.

**3. Add one PNG per item**, named after it:

| Item | File |
|---|---|
| All Downloads | `all-downloads.png` |
| Unfinished | `unfinished.png` |
| Finished | `finished.png` |
| Compressed | `compressed.png` |
| Documents | `documents.png` |
| Music | `music.png` |
| Programs | `programs.png` |
| Video | `video.png` |
| General | `general.png` |
| Any other category | its name in lower case, spaces as `-` (a category "Mobile Apps" is `mobile-apps.png`) |
| Every category without its own file | `category.png` |
| Grabber projects | `grabber-projects.png` |
| A grabber project | `grabber-project.png` (otherwise `grabber-projects.png`) |
| Queues | `queues.png` |
| A queue | `queue.png` |
| A queue that is running | `queue-running.png` (otherwise `queue.png`) |

Pictures should be square with a transparent background, 64 × 64 pixels or larger so they stay sharp at higher
**View → Custom scale** sizes; NDM fits each one into the row. An item without a picture keeps NDM's own icon, so a
set can change just a few.

```
category-icons/
└── Candy/
    ├── all-downloads.png
    ├── finished.png
    ├── video.png
    ├── category.png
    └── queue-running.png
```

**4. Choose it:** **View → Category icons → Candy**. **Built-in icons** goes back to NDM's own. Choosing a set again
reloads its pictures after you change them.

To share a set, zip its folder; whoever gets it unzips it into their category icons folder. You can post sets for
others in [Issues](https://github.com/MoeJoe37/New-Download-Manager/issues). **Help → Look for new category icons**
opens this guide.

## Building from source

The [architecture guide](docs/architecture.md) maps features to their owning modules and explains dependency and visibility rules. The [contributor guide](CONTRIBUTING.md) covers setup, one-command checks and performance reports.

Requirements: Windows 10/11 (x64 or ARM64), [Rust](https://rustup.rs) (stable, MSVC toolchain), the Windows
SDK (for the exe icon).

```sh
cargo build --release          # target\release\ndm.exe
cargo test --release           # tests
```

Installer ([Inno Setup 6](https://jrsoftware.org/isinfo.php)); `/DArch=arm64` makes the ARM64 one from
`target\aarch64-pc-windows-msvc\release\ndm.exe`:

```sh
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\ndm.iss   # installer\out\NDM-Setup-<version>.exe
```

| Folder | What |
|---|---|
| `src/` | The app (egui UI, download engine, streams, grabber) |
| `extension/` | Browser extension (Manifest V3) |
| `help/` | Help pages, shown inside the app |
| `Languages/` | Language files |
| `installer/` | Inno Setup script |
| `tools/` | `ndm_lng.py` (language file generator), `icon.py` (app icon), `appimage.sh`, `linux-packages.sh` and `macos.sh` (packages), `smoke*` (start-up tests CI runs) |

Linux packages (on Linux, x86_64 or aarch64, or in WSL on Windows; needs `build-essential pkg-config
libgtk-3-dev libxdo-dev libayatana-appindicator3-dev`, `rpm` for the .rpm, and Rust):

```sh
bash tools/appimage.sh                                  # installer/out/NDM-<arch>.AppImage
bash tools/linux-packages.sh target/release/ndm         # .deb, .rpm and .tar.gz
```

macOS (Xcode command line tools and Rust with both Mac targets:
`rustup target add aarch64-apple-darwin x86_64-apple-darwin`):

```sh
bash tools/macos.sh            # installer/out/NDM-<version>-macOS.dmg, one app for Apple Silicon and Intel
```

GitHub Actions builds and tests all of them (`.github/workflows/ci.yml`) and attaches them to a release when
a `v<version>` tag is pushed (`release.yml`).

## Translating

`Languages/ndm_en.lng` lists every text NDM shows, one `English=Text` per line. It is generated from the
source by `python tools/ndm_lng.py` (`--check` tells whether it is up to date, and whether every translation
keeps its placeholders).

NDM comes with Spanish (`ndm_es.lng`), French (`ndm_fr.lng`) and Arabic (`ndm_ar.lng`). To add a language,
copy `ndm_en.lng` to `ndm_xx.lng`, change the first line (`lang=0x407 Deutsch`), and translate the text after
each `=`. Keep `%s` / `%d` and `\n`, in the same order. A string you leave out shows in English, and a text
too long for its place is drawn smaller. The `%b %d %H:%M:%S %Y` line is the date format (`%d/%m/%Y` for
day/month/year). NDM lists the file under **View → Language**.

Files named `ndm_ar`, `ndm_fa`, `ndm_he` or `ndm_iw` are right to left: NDM mirrors every window and lays the
text out itself (letters joined, words in reading order), so the translation is written as usual.
Pull requests welcome.

## License

[GNU Affero General Public License v3.0](LICENSE). The toolbar and tree icons are drawn at run time from
Windows' own icon font (Segoe Fluent Icons); on Linux from Google's Material Icons
(`assets/linux`, Apache License 2.0).
