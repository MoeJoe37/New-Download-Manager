# NDM code-splitting plan

**Baseline:** NDM 2.9.1 at `fd1f478`. Today `main`, `V-codex` and `claude/determined-brown-7lh8eh` all point there and nothing is in flight, which makes now the right moment to start.

**Based on:** the reviewed *NDM code-splitting plan*. This plan keeps its module map, its order and its traps. It adds the parts that turn "review carefully" into "the tools prove it": a coordination rule, automatic equivalence checks, CI parity, a formatting guard, shared test fixtures, measurable targets, a numbered list of changes, and a rollback rule that still works once moves stack up. The extension split moves to a second pass.

**Goal:** give each feature a predictable home, in changes small enough to review, test and revert one at a time. The binary, the browser extension, the saved state, the partial-download formats and the packaging stay exactly as they are.

## 1. What "done" means

The first pass is done when all of these hold:

| Target | Today |
|---|---|
| `src/main.rs` is at most 150 lines: crate attributes, `mod` declarations, `fn main` | 9,015 lines |
| `App::dialog_body` is a dispatcher of at most 150 lines, one call per dialog | 3,390 lines |
| Each dialog family module is at most 800 lines | the largest dialog, Options, is 499 lines |
| No Rust file has more than 1,200 lines of non-test code, except `remux.rs` (its split is the follow-up) | `main.rs`, `engine.rs`, `core.rs`, `ytdlp.rs`, `stream.rs` exceed it |
| No temporary compatibility re-export is left | — |
| The language catalogue, the test inventory and the dialog snapshots (section 3) are unchanged by every move | — |
| CI is green on Windows x64 and ARM64, Linux x86_64 and aarch64 and macOS, including the 15 distribution smoke tests | green at `fd1f478` |

Non-test lines are the file minus its `#[cfg(test)]` items, counted with the scanner's `without_tests` (`tools/ndm_lng.py`).

Sizes at the baseline:

| File | Lines | What separates out |
|---|---:|---|
| `src/main.rs` | 9,015 | Startup, updater, diagnostics, help, events, window views, about 70 dialogs |
| `src/engine.rs` | 2,864 | Job execution, byte ranges, probes, FTP, HTTP agents and sign-in |
| `src/core.rs` | 2,463 | Settings and state, browser bridge, queues and limits, routing |
| `src/ytdlp.rs` | 2,078 | Tool installation, format choice, playlists, download and merge |
| `src/stream.rs`, `src/remux.rs` | 1,893, 1,780 | HLS, DASH, fragment download, fMP4; TS demuxing, codecs, writers |
| `src/win.rs`, `src/linux.rs`, `src/mac.rs` | 1,046, 1,000, 790 | Native integration per concern |
| `extension/background.js`, `content.js` | 521, 481 | Deferred (section 8) |

## 2. Rules for every change

### Two kinds of change

- **A move** has a subject starting with `move:`. It only relocates code. Allowed edits: `mod` and `use` lines, visibility (`pub(super)`, `pub(crate)`), temporary re-exports, and the paths in `#[path]` or `include_bytes!` when the move needs them. Not allowed: renames, new logic, reformatting, rewritten comments. Comments travel with their code.
- **Everything else** (preparation, fixes, features) is a normal change, reviewed as code.

A change is one branch with one squashed commit, built from the current `main`.

### Coordination

Almost every feature touches `main.rs`, and two agents push to two branches. A feature edit to a function that a move just relocated becomes a conflict where a careless resolution silently drops one side. So:

1. The user starts each phase. While a phase runs, nobody changes the files that phase splits (section 10 lists them). Other files are fine.
2. One move at a time per file. One agent moves, the other reviews with the checklist in section 9, and the user fast-forwards `V-codex` and `main`.
3. A move lands within a day. If anything else landed in the moved files meanwhile, **redo the move on the new base**. Never resolve conflicts by hand inside moved code; moves are mechanical, so redoing one is cheap.
4. Releases happen at phase boundaries, after the release smoke on all three systems. An urgent fix can still ship from `main` at any time, because every change is green.

### Code rules

- **Visibility:** the narrowest that compiles. `App`'s fields stay private in `src/app/mod.rs`; its child modules can see them.
- **Attributes:** crate attributes stay in `main.rs`. `#[cfg]` attributes move with their items, including the macOS-only dialog arms.
- **Tests:** they move with their code into a `#[cfg(test)] mod tests` block in the same file, which the scanner already skips. A separate test file is only for large fixtures, and goes on the scanner's exclusion list (A2).
- **Variable names:** until A2 lands, keep `d` and `d2` as the names of `dlu::D` values. The scanner finds `d.check(.., "text")` but misses the same call written `dlg.check(.., "text")`.
- **Re-exports:** each temporary one gets a `// split: re-export, removed at the end of phase X` comment. The last change of each phase removes the ones it added and updates the callers.
- **Locks:** keep one `settings()` / `info()` lock per statement, and keep the order in which they're taken.

## 3. Automatic gates

Every change runs all of these. CI runs them on every push, not only at review.

| Gate | Command | Rule |
|---|---|---|
| Build, tests, lint | `cargo build --release --locked`, `cargo test --release --locked`, `cargo clippy --release --locked --all-targets -- -D warnings` | Green on Windows, Linux and macOS. macOS clippy is new (A1). |
| Language catalogue | `python3 tools/ndm_lng.py --check` | A move leaves `Languages/` untouched. A difference means the scanner gained or lost strings: fix the code or the scanner, never regenerate in a move. |
| Test inventory | `python3 tools/test_inventory.py --check` | The test names (without module path), each marked ignored or not, equal `tools/snapshots/tests.txt`. A move leaves it unchanged. Baseline: 137 tests, 11 of them ignored. |
| Dialog snapshots | `cargo test --release dialog_snapshots -- --ignored --test-threads=1` (Linux, `TZ=UTC`) | Every dialog's frame equals `tools/snapshots/dialogs.txt` (A5). A move leaves it unchanged. |
| Move-only | `tools/check-move.sh <base>` | For `move:` commits: nothing changed under `Languages/`, `tools/snapshots/`, `assets/`, `help/`, `extension/`, `installer/`, `.github/`, nor in `Cargo.toml` or `Cargo.lock`. |
| Whitespace | `git diff --check <base>` | Clean. |
| Extension | `node tools/test-extension.mjs`, `node tools/test-panel.mjs` | Both green. `test-panel.mjs` runs in CI from A1 on (today it never runs there). |
| Real tools | the existing CI steps: `real_ytdlp_separate_streams`, `real_ytdlp_audio_and_extras`, `real_ffmpeg_install`, `actual_media_` | Green. CI selects these tests by name, so the names must not change. |
| Start-up | `smoke-windows.ps1`, `tray-windows.ps1`, `smoke.sh` on 15 distributions, `smoke-macos.sh` | Green. |

The reviewer also reads the move with `git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change <base>`. Dialog code moves from 16-space indentation inside `dialog_body` to 8-space method bodies, and plain `--color-moved` shows every re-indented line as new code.

## 4. Phase A — preparation (5 changes, no moves)

**A1. CI parity and guards**
- Add `--locked` to every cargo command in CI, and clippy to the macOS job. `mac.rs`, `mac_capture.rs` and the macOS-only dialog arms will move, and macOS can't be checked locally because `ring` needs Apple's toolchain.
- Add an `extension` job on `ubuntu-latest` that installs Node, Playwright's Chromium (`npx playwright install --with-deps chromium`) and FFmpeg, then runs both extension tests.
- Add `rustfmt.toml` with `disable_all_formatting = true`. The code isn't rustfmt-formatted (`cargo fmt --check` reports 1,735 differences), and one format-on-save would bury a move. The option is stable and honoured by `cargo fmt` and rust-analyzer.
- Add an empty `.git-blame-ignore-revs`, with a comment explaining it is for a future formatting commit. GitHub reads it automatically.
- Add `tools/check-move.sh` and run it in CI.

**A2. Translation scanner**
- Discover `src/**/*.rs` recursively. Today it reads only `src/*.rs`.
- Find `dlu` calls whatever the variable is called.
- Exclude test-only files by name: `src/transport_bench.rs`, `src/test_support.rs`, and any `tests.rs`. They have no `#[cfg(test)]` of their own; the gate sits on the `mod` line elsewhere.
- Proof: the generated `ndm_en.lng` is byte-identical before and after. If the wider search finds new strings, decide each one in this change: route it through `tr()` or exclude it, and say which in the commit.

**A3. Path independence**
- **Crash diagnostics:** map nested source paths (`src/app/help.rs`, and `src\app\help.rs` on Windows) to labels such as `app::help`. Keep accepting the old flat headers (`engine:42`), and keep the allowlist and redaction. Extend `problem_report_excludes_raw_crash_data` with a nested path, an old flat header, Windows separators and a rejected private path.
- **Assets:** write the five `include_bytes!` (four in `main.rs`, one in `linux.rs`) as `include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/…"))`, so they keep working from any directory.

**A4. Shared test fixtures**
- Add `src/test_support.rs` behind `#[cfg(test)]`. It holds the local HTTP server (`engine::tests::serve`), the media fragment builders (`stream::tests::{frag, init}`) and the TLS server setup used by the transport tests.
- Point the two tests that reach into other modules at it (`ytdlp.rs` lines 1665 and 1913). Otherwise they break when `engine` or `stream` splits.
- Merge duplicated hand-written servers only where they are identical; no test changes what it checks.

**A5. Dialog snapshots and test inventory**
- **Split `App::new`:** state construction becomes `App::build(ctx, core, tray, rx)`. The start-up side effects stay in `App::new`, in the same order: the integration proxy, the background thread, the yt-dlp updates, the local server and the tray. This is a code change, so it gets the tray and start-up smoke tests.
- **Fonts:** move font loading out of `style()`, so a test can use egui's built-in fonts. `style()` reads Windows fonts and Linux fallback fonts from the system.
- **Titles and sizes:** extract the table of titles and template sizes at the top of `App::dialog` into `fn frame(d: &Dialog) -> (String, Option<Vec2>)`.
- **`dialog_snapshots` test:** ignored by default and run in its own Linux CI step, because the language tables are global.
  - It builds a Core from `Saved::default()` on a temporary store, with three fixed jobs (stopped, failed, complete; fixed dates).
  - It takes one sample of every dialog from an exhaustive `match` with no wildcard, so a new dialog can't be left out.
  - It renders one frame each at time 0, 1.0 pixels per point, light theme, English.
  - The dump holds the title and size, every text with its rect rounded to 0.5 px, and every other shape's kind and rect (images without texture ids).
  - It compares the dump with `tools/snapshots/dialogs.txt`; `NDM_BLESS=1` rewrites the file.
  - The repo's existing headless egui tests (`file_info_renders_both_logical_hebrew_names`, `dlu::combo_closes_after_selecting_a_choice`) show the approach works.
- **`tools/test_inventory.py`** and its file `tools/snapshots/tests.txt`.

**Gate:** everything in section 3 is green, the catalogue is identical, and the test count only grows by the tests added here.

## 5. Phase B — the desktop app (11 moves)

Frozen during this phase: `src/main.rs`, and the files under `src/app/` as they appear.

| # | Moves | To |
|---|---|---|
| B1 | Help: parsing, search, rendering, `help_pages` test. The Help menu item stays. | `src/app/help.rs` |
| B2 | Updater: `check_update`, `update_asset`, `install_update`, `update_assets` test. Diagnostics: panic hook body, `crash_location`, `crash_diagnostics`, `issue_text` and their tests. | `src/update.rs`, `src/diagnostics.rs` |
| B3 | `Res`, `Toolbar`, icons, fonts, `style()` | `src/app/resources.rs` |
| B4 | `Row`, `Row::cell`, the download table | `src/app/window/table.rs` |
| B5 | Category tree, menus, toolbar, status bar | `src/app/window/{categories,menu,toolbar}.rs` |
| B6 | `Dialog`, `frame()`, the viewport code of `App::dialog`, `dialog_body`, unchanged | `src/app/dialogs/mod.rs` |
| B7 | Small families: checksum, playlist, problem report, speed limit, HTTP/1.1 hosts, macOS capture. Each `dialog_body` arm becomes one call. | `src/app/dialogs/{checksum,playlist,report,limits,mac}.rs` |
| B8 | Download dialogs: File Info (188 lines), Progress (174), Complete, Duplicate, Delete, File Properties | `src/app/dialogs/download.rs` |
| B9 | Options (499), one function per tab | `src/app/dialogs/options/` |
| B10 | Scheduler (228), Grabber (282) with its schedule, settings and login dialogs, then the rest: help viewer (242), browser panel (112), new version (102), toolbar customizing, proxy, columns… | `src/app/dialogs/{scheduler,grabber,help_view,browser,misc}.rs` |
| B11 | `App` and `impl eframe::App`, then start-up (`run`, `tray_start`, renderer choice, single instance). `main.rs` keeps the crate attributes, the `mod` lines and `fn main`. | `src/app/mod.rs`, `src/app/events.rs`, `src/startup.rs` |

**By hand, after B8, after B11 and at the end of the phase:**
- In File Info: start and cancel, the quality drop-down, the Hebrew name.
- Duplicate downloads.
- The progress and completion popups.
- Tray start-up.
- Switching theme and language.

`tray-windows.ps1` runs in CI, because event order can reopen the main window.

The phase ends by removing its re-exports and tagging `split-b-done`.

## 6. Phase C — the backend (9 moves)

Frozen per move: the file being split.

| # | Moves | Keep exactly | Tests that guard it |
|---|---|---|---|
| C1 | Settings, defaults, `Saved`, load and save, migration → `src/core/{settings,state}.rs` | field names, serde attributes, defaults, queue ids, the state path | `core::tests::old_state_keeps_behaviour`, cross-version check |
| C2 | Browser bridge: `Incoming`, request parsing, `spawn_server`, feeds → `src/core/browser_bridge.rs` | loopback binding, port, accepted origins, size limits, message format | `core::tests::localhost_rejects_web_origins_and_ambiguous_messages`, `test-extension.mjs`, a real Chrome and Firefox handoff at phase end |
| C3 | Queues, scheduler, quota, limits → `src/core/queue.rs` | lock order, queue ids | `core::tests::queues`, `core::tests::download_limits`, `core::queue_speed_limit` |
| C4 | Per-address routing and network lanes → `src/core/routing.rs` | proxy choice per address | `core::tests::proxies`, `core::tests::lanes_only_for_direct` |
| C5 | FTP → `src/engine/ftp.rs`; probes and range checks → `src/engine/{probe,range}.rs` | range validation, permanent-error rules | `engine::tests::ftp_*`, `engine::tests::rejects_invalid_ranges_and_preserves_partial_when_etag_changes` |
| C6 | Job execution: `Job::run`, `worker`, `fetch`, `steal` → `src/engine/job.rs` | retry and connection rules | `engine::tests::downloads_resumes_and_splits`, `steal_keeps_segments_contiguous`, `resume_detects_changed_file`, `keeps_to_a_servers_connection_limit` |
| C7 | HTTP agents and TLS (`tls_config`, `agent`, `agent_with_tls`, `tls_client`, `call_with`) and sign-in (`call_legacy`, `sspi`, NTLM, GSS) → `src/transport/{client,auth}.rs`. `engine` keeps re-exports. This removes the dependency from `transport` back into `engine` (`Agent::for_url` calls `crate::engine::agent` today). | proxy routes, redirects, HTTP/2 first request, HTTP/1.1 extra connections with HTTP/2 fallback | `transport::tests::*`, `transport::benchmark::extra_requests_can_use_http1_without_downgrading_the_first`, `transport::benchmark::extra_request_retries_with_http2_and_remembers_the_origin`, plus a `http_protocol_benchmark` run compared with the baseline |
| C8 | yt-dlp: installation and updates → `src/ytdlp/install.rs`; `formats`, `chosen`, `named` → `formats.rs`; playlists → `playlist.rs`. `run`, `fetch_streams`, `join`, `restore_tracks` and the subprocess lifecycle stay in the root. | `source_identity`'s input string (merge receipts), strict quality caps | `ytdlp::tests::quality_choices`, `failed_merge_keeps_sources_and_reports_error`, the real yt-dlp CI steps |
| C9 | Streams, in two moves: HLS and DASH parsing → `src/stream/{hls,dash}.rs`, then fragment download and fMP4 assembly → `src/stream/{download,fmp4}.rs` | fragment fingerprints | `stream::tests::*`, the `actual_media_` CI step |

**Cross-version check** after C1, C6, C8, C9 and at the end of the phase. Using 2.9.1 from the Releases page:
1. Start three downloads: a multi-connection file, a yt-dlp video with separate audio, and an HLS stream.
2. Stop each about half-way and quit.
3. Start the new build. Each download resumes without fetching again what it had and completes.
4. The saved state loads with the same queues, limits and folders.

The phase ends by removing its re-exports and tagging `split-c-done`.

## 7. Phase D — platform code (up to 3 moves)

- **The alias stays:** `#[cfg_attr(target_os = "linux", path = "linux.rs")] #[cfg_attr(target_os = "macos", path = "mac.rs")] mod win;` keeps choosing the platform.
- **One platform per move**, splitting only coherent units: native messaging host and desktop entries, proxy and VPN, drag and drop.
- **Unsafe code stays together:** callbacks and COM or Objective-C code stay next to the code that owns their lifetimes and threads.
- **macOS:** CI compiles and lints it (A1), but a run on a real Mac is a separate acceptance step before release: HTTP capture on and restored, the drop target, the menu bar icon.

## 8. Deferred: the browser extension

The two files are about 500 lines each, so the payoff is small. Splitting them carries real Manifest V3 risk, so it waits for a second pass. If it's done later, the mechanism is fixed now:

- **Background:**
  - Firefox: `background.scripts` lists the parts, ending with `background.js`.
  - Chrome: `background.js` starts with `if (typeof importScripts === "function") importScripts(…)`. Chrome's service worker has `importScripts`; Firefox's background page doesn't.
  - The listeners stay registered at the top level, as today (`background.js` lines 190, 302, 303, 337, 428, 453).
- **Content scripts:** an ordered `content_scripts[0].js` list sharing the isolated world. `page.js` stays in the page's world.
- **Tests:** they read the script lists from `manifest.json`. Today `test-extension.mjs` loads exactly `background.js` into a vm, and `test-panel.mjs` injects only `content.js`.
- **Before merging:** load the unpacked extension in Chrome and Firefox.

## 9. Review checklist

1. The subject starts with `move:` and `tools/check-move.sh` passes.
2. In `git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change <base>`, everything is dimmed except `mod` and `use` lines, visibility and re-exports.
3. There are no renames and no new logic, and comments arrived intact.
4. Locks are taken in the same order, one per statement.
5. Visibility is the narrowest that compiles.
6. Test names are unchanged, so CI's name filters still select the same tests.

## 10. Order, size and freezes

| Phase | Changes | Moves | Frozen |
|---|---:|---|---|
| A | 5 | no | nothing (each change touches few files) |
| B | 11 | yes | `src/main.rs` and the new `src/app/` files |
| C | 9 (C9 is two moves) | yes | the file being split |
| D | up to 3 | yes | the platform file being split |
| **Total** | **about 29** | | |

**First slice:** A1 to A5, then B1 (help). B1 is the small first move that shows the gates, the review and the redo-on-conflict rule work before the larger dialog moves.

## 11. History and rollback

- **Fix forward:** moves are behaviour-free and checked by the gates, so a regression found later is most likely an accidental edit. Fix it with a normal change.
- **Reverting:** a move is one squashed commit, so it reverts cleanly while it is the latest. Once later moves build on it, don't revert; fix forward.
- **Bisecting:** tags at the end of each phase (`split-a-done`, `split-b-done`, `split-c-done`, `split-d-done`) give `git bisect` fixed points.
- **Blame across files:** GitHub's blame doesn't follow lines into another file. Use `git blame -C -C <file>`, which finds lines moved from other files in the same commit, and `git log -L` or `git log --follow` for whole-file moves.
- **Formatting:** if the code is ever formatted, do it as one separate change at the very end, listed in `.git-blame-ignore-revs`.

## 12. What this adds to the reviewed plan

1. **Coordination:** a freeze per phase, one move in flight per file, redo-on-conflict, and releases at phase boundaries.
2. **Automatic proof that a move changed nothing:**
   - the test inventory;
   - the byte-identical catalogue, never regenerated in a move;
   - dialog snapshots;
   - an indentation-tolerant moved-code diff.
3. **CI parity:** clippy on macOS, `test-panel.mjs` in CI, `--locked` everywhere.
4. **A formatting guard:** `rustfmt.toml` with `disable_all_formatting = true`.
5. **Shared test fixtures** before any module that owns them splits.
6. **The extension split deferred,** with its loading mechanism fixed for later.
7. **Measurable targets and a numbered list of about 29 changes.**
8. **A rollback rule that works once moves stack up:** fix forward, squashed moves, phase tags.
