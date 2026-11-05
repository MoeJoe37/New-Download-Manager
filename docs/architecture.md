# Code ownership and dependencies

NDM is one Rust application with a browser extension. Keep each feature's state,
execution, and user interface in its owning module. Prefer named imports so a
module's dependencies are visible at the top of its file.

| Module | Owns | Main callers |
| --- | --- | --- |
| `startup` | Arguments, single instance, renderer fallback and process startup | `main` |
| `app` | Desktop state, events, windows and user actions | `startup` |
| `app::dialogs` | Dialog routing, lifetime, layout and feature-specific forms | `app` |
| `app::resources`, `app::display` | Fonts, icons, themes and display formatting | Desktop views |
| `core` | Shared application coordination and background work | Desktop and browser bridge |
| `core::settings`, `core::state` | Preferences, saved-data compatibility and persistence | `core` and desktop preferences |
| `core::queue` | Scheduling, quotas, queue policy and job limits | Background coordinator |
| `core::routing` | Proxy selection, network lanes and capture integration | Coordinator and transport setup |
| `core::browser_bridge` | Loopback endpoints, browser messages and page-fed downloads | Startup and extension |
| `engine` | Download state, progress and job lifecycle | Coordinator, streams and desktop |
| `engine::range`, `engine::probe`, `engine::ftp`, `engine::names` | Range rules, probes, FTP and file names | Download execution |
| `transport` | Requests, responses, redirects, connection policy and authentication | Engine, streams and tool downloads |
| `stream` | HLS/DASH plans, fragment transfer and fragmented MP4 assembly | Engine and media downloads |
| `remux` | Transport-stream parsing, codecs and native MP4/WebM writers | Streams and media processing |
| `ytdlp` | Tool lifecycle, format policy, playlists and video download coordination | Desktop, coordinator and engine |
| `media` | Media inspection, staging, validation and external muxing | Engine and video downloads |
| `grabber` | Site exploration and offline conversion | Coordinator and grabber UI |
| `update`, `diagnostics` | Application updates and redacted problem reports | Startup and desktop |
| `win` | The platform API selected by `main` for Windows, Linux or macOS | Startup, desktop and routing |
| `platform/<system>/` | Desktop integration, network integration and native drops | That system's `win` implementation |

The engine and stream modules share job and progress types. Keep protocol policy
in `transport` and fragment policy in `stream`. The desktop requests work through
`core` and `Job` methods. Native callbacks and their ownership rules stay together
inside the selected platform implementation.

## Import and visibility rules

- Name the symbols imported from a parent or sibling. Production modules use
  explicit imports instead of `use super::*` or `use crate::app::*`.
- Import platform-only names under the same target condition as their use.
- Keep helpers private. Use `pub(super)` for sibling collaboration and
  `pub(crate)` for interfaces used across application subsystems.
- Keep tests with the behavior they exercise. Test modules may import their
  owner's private implementation with `use super::*`.
- Keep the `win` facade stable across systems. Windows API imports belong in the
  native module that calls those APIs.
- Add a re-export when it defines a lasting subsystem interface. Update callers
  when moving an implementation detail to its owner.

## Browser extension

`background.js` registers listeners in a fixed order. Chrome synchronously loads
its helpers with `importScripts`. Firefox loads the same helpers from the ordered
`background.scripts` manifest list before the entry point.

| Script | Owns |
| --- | --- |
| `background/desktop.js` | Desktop discovery, native launch and requests |
| `background/downloads.js` | Download decisions, interception and page-fed transfer |
| `background/media.js` | Per-frame media tracking and navigation cleanup |
| `background/menus.js` | Configuration and context menu actions |
| `content.js` | Keyboard reporting, video-page resolution and link queries |
| `content/player-panel.js` | Media player controls and format selection |
| `content/selected-links.js` | Selected-link controls |
| `content/page-feed.js` | The isolated-world side of page-fed transfer |
| `page.js` | Fetching and streaming in the page's MAIN world |

Content scripts share the isolated world in manifest order. The MAIN-world script
uses the existing message protocol to communicate with the isolated script.
Browser fixtures read the shipped manifest so script wiring is tested with the
same file lists used by the installed extension.

## Changing a feature

Start with its owner in the tables above. Preserve saved-data fields and migration
defaults when changing settings or download records. Preserve validators, ranges,
fragment identities and merge receipts when changing resume behavior. Exercise
browser messaging with the extension fixtures when changing the bridge.

Keep formatting changes in a dedicated commit recorded in `.git-blame-ignore-revs`
so behavior changes remain readable in history.
