# Contributing to NDM

The [architecture guide](docs/architecture.md) maps each feature to its owner and
explains the dependency and visibility rules. Keep behavior changes and formatting
in separate commits. Add regression coverage when fixing a reproducible bug.

## Prerequisites

Install Rust through rustup and Python 3.8 or newer. The repository selects its supported
Rust compiler and includes Clippy and rustfmt through `rust-toolchain.toml`.
Cargo commands use the checked-in lock file.

| System | Additional build prerequisites |
| --- | --- |
| Windows | MSVC build tools and the Windows SDK |
| Linux | `build-essential pkg-config libgtk-3-dev libxdo-dev libayatana-appindicator3-dev` |
| macOS | Xcode command line tools |

Browser and media checks also need Node.js 22 or newer, Firefox, FFmpeg and
ffprobe. Install the pinned Playwright package and its Chromium build from the
repository root.

```sh
npm install --no-save --package-lock=false playwright@1.62.1
npx playwright install --with-deps chromium
```

Firefox tests install a temporary add-on into a disposable browser profile. They
reserve the bridge's candidate ports so the fixture cannot contact a running NDM.

## One-command checks

Run the Rust, formatting, module, translation and applicable desktop checks.

```sh
python3 tools/check.py
```

Include the installed-browser workflows and FFmpeg integration when changing the
browser bridge, extension or media handling.

```sh
python3 tools/check.py --browsers
```

Use `--release` to check the optimized build. Pass `--firefox /path/to/firefox`
when Firefox is outside PATH. On Windows, use `python` for the Python commands.

CI also tests real yt-dlp and tool installation, builds every package, exercises
Windows tray behavior and runs distribution startup smoke tests. Native macOS
proxy restoration and desktop drag-and-drop need a manual acceptance check on
those systems.

## Formatting and history

```sh
cargo fmt --all
cargo fmt --all -- --check
git config blame.ignoreRevsFile .git-blame-ignore-revs
```

The ignore file records the dedicated formatting commit. Keep named application
imports explicit. Test-only files use `tests.rs`, `*_tests.rs`, or the shared
`test_support.rs` convention so fixtures stay out of the translation catalogue.
New user-facing strings need Arabic, Spanish and French translations alongside
the generated English catalogue. Update the test inventory deliberately when
adding or removing a Rust test.

```sh
python3 tools/ndm_lng.py
python3 tools/test_inventory.py --write
```

## Performance reports

```sh
python3 tools/benchmark.py --output artifacts/performance.json
python3 tools/benchmark.py --baseline artifacts/previous.json --output artifacts/current.json
```

The fixtures validate downloaded bytes and report native range throughput, HTTPS
throughput and TLS connection counts. Reports also include elapsed time and,
where the operating system supplies them, CPU time, peak resident memory and
block I/O. CPU and I/O figures cover the test process and fixture server threads.
Compare the same profile and environment. CI publishes reports as artifacts so
changes can be compared without imposing timing thresholds on shared runners.

On a disposable Linux desktop, add `--app target/release/ndm` to measure three
startups through the first marked bridge response. The application runs its normal
startup, including desktop integration. CI supplies Xvfb and an isolated runner.
The report includes all three observations and their median. Use `--debug` only
when measuring a debug test executable.
