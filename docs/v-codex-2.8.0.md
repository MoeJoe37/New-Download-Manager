# V-codex 2.8.0 changes

The branch incorporates the six upstream commits through `df44ae6`, including the 2.7.5 FFmpeg
installer and the Windows popup regression. The desktop app and bundled extension share version
2.8.0 so the extension can reload when the app distributes the new message protocol.

## Browser and media changes

- The player panel reacts to dynamic insertion, metadata and source changes, navigation, and
  player removal. It coalesces mutation and layout work into animation frames. The existing
  protected-content preference can show a protected-player notice. That notice offers no download.
- Response detection uses MIME types alongside filenames, including extensionless MP4, TS, MKV,
  HLS and DASH resources. It scopes stored responses by frame and document, filters known ad
  requests and fragment responses, and deduplicates repeated ranges or equivalent signed URLs.
  Format settings filter the current list without disabling transfers or deleting files.
- The desktop reads authorized manifests on request. The panel offers known HLS variants and
  single-period on-demand DASH representations with available resolution, bitrate, codecs and
  audio metadata. Compatible HLS streams can offer TS and MP4. Standalone TS sources are sampled
  before offering MP4 or MKV, and the panel distinguishes MP4 conversion from remuxing.
- Media processing writes a staged output and publishes it after validation. FFmpeg copies
  compatible streams and selectively encodes incompatible tracks when MP4 conversion is
  requested. FFprobe checks container, audio and video packets, resolution, duration, and
  available start offsets. MKV mapping preserves subtitle tracks, language metadata and chapters
  carried by the local source. Existing native fragmented MP4 and WebM assembly remains available.
- Combined media jobs fail if they cannot produce the required tracks. yt-dlp records completed
  video and audio tracks so a merge can be retried without extracting or downloading them again.
  VOD fragments remain on disk and can be reused for the same manifest plan. Plain fragments
  stream directly to disk with bounded buffers. The native AES-128 path caps buffered encrypted
  fragments at 64 MiB. FFmpeg progress uses timestamps reported by the process.

## Networking, recovery and desktop changes

- Direct HTTPS uses a pooled HTTP/2 client with TLS ALPN and HTTP/1.1 negotiation. The existing
  HTTP/1.1 transport remains in use for configured proxies, network-bound routes, Digest, NTLM,
  and Negotiate. Same-origin redirects retain request context. Cross-origin redirects drop
  credentials and custom headers, and media cookies stay scoped to the original origin.
- Validates 206 bounds and lengths before writing. A server that ignores Range can supply a new
  file sequentially. An incompatible response cannot be appended to saved segments. Resume
  compares ETag and Last-Modified when available, checks saved range layout and file length, and
  retains the existing boundary-byte verification. A detected identity change preserves the
  partial until an explicit restart or user-initiated resume.
- Permanent HTTP failures such as 403 and 407 stop automatic retries. Transient segment failures
  back off with bounded jitter, and queue retry counts remain configurable. Failed PAC resolution
  reports a route error, while explicit PAC DIRECT and supported SOCKS results retain their
  routing semantics. yt-dlp uses the selected download route. Diagnostic errors omit signed URL
  paths and query strings.
- The extension discovers the desktop through a marked localhost response, applies bounded
  connection timeouts, and avoids replaying a request whose acceptance is uncertain. The
  desktop validates extension origins, message size, header framing and supported output formats.
- Actual media signatures correct conflicting media suffixes. Shell folder selection runs in a
  separate COM apartment with balanced initialization and cleanup. There is no component in
  this repository that loads into Explorer's process.

## Files changed

- `src/transport.rs`, `src/engine.rs`, `src/net.rs`, `src/posix.rs`, `src/core.rs` and
  `src/grabber.rs` cover transport, routes, response validation, recovery and the localhost API.
- `src/stream.rs`, `src/media.rs`, `src/ytdlp.rs`, and `src/main.rs` cover media choices,
  assembly, processing progress, and the desktop handoff.
- `extension/background.js` and `extension/content.js` cover detection, lifecycle, connection
  recovery and the panel. `src/win.rs` covers Explorer folder selection.
- `Cargo.toml`, `Cargo.lock`, the extension manifest and installer carry the version and
  dependencies. `Languages/ndm_en.lng` and its generator reflect the new status text.
- `tools/test-extension.mjs`, `tools/test-panel.mjs` and `.github/workflows/ci.yml` cover browser
  regressions and integration tests. Rust fixtures live beside the affected modules.

## Verification and remaining limits

- The default suite passed 112 tests locally. Run `cargo test --locked` to reproduce it. Its local TLS fixture checks ALPN, two
  simultaneous requests over one connection with a server limit of two, and reconnect after
  GOAWAY. Local servers exercise redirects, proxy authentication, 403, partial recovery and
  ignored ranges. The Windows x64 target and its tests passed a cross-compilation check.
- Run `cargo test --locked actual_media_ -- --ignored --test-threads=1` with FFmpeg and ffprobe.
  The fixtures use real H.264/AAC and legacy audio in TS, remux and convert to MP4, mux separate
  tracks into MKV, and download selected HLS qualities. The local two-second TS fixture took
  approximately 0.40 seconds per remux, including validation. The measured audio/video
  start-offset difference was 0.667 ms for MP4 and 0.333 ms for MKV. These measurements are for
  the fixture and do not establish a throughput improvement on production sites.
- Run `NDM_TEST_YTDLP=/path/to/yt-dlp cargo test --locked real_ytdlp_separate_streams -- --ignored --test-threads=1`
  for the real HLS/DASH yt-dlp fixture. Run `node tools/test-extension.mjs` for the background
  regressions. Run `node tools/test-panel.mjs` with Playwright, Chromium and FFmpeg for the actual
  browser DOM fixture. The latter uses a controlled extension messaging boundary.
- Generic video detection still depends on accessible responses and metadata. Ads and signed
  URLs without stable identity cannot always be distinguished. Multi-period and live DASH retain
  automatic quality selection. Validation checks container and packets, and does not decode
  every frame or certify every possible player and codec combination.
- HTTP/2 is limited to direct HTTPS in this implementation. Proxy and VPN tests use controlled
  routes, not every provider. Explorer COM changes were cross-compiled. Windows CI runs the
  existing Explorer path regression. No specific Explorer crash was reproduced. Disk-write
  exhaustion was injected in a fixture. A real full disk, all browser/OS combinations,
  and end-to-end performance on production websites were not verified.
