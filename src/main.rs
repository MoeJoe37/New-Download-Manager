#![windows_subsystem = "windows"]
// Dialogs are laid out in dialog units: their helpers take x, y, width, height and the text, and the UI keeps
// its state in a few nested shared structures. Those two lints would only rename that.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]
mod app;
mod browsers;
mod checksum;
mod core;
mod diagnostics;
mod dlu;
mod engine;
mod file_ops;
mod grabber;
mod i18n;
#[cfg(target_os = "macos")]
mod mac_capture;
mod media;
mod net;
#[cfg(unix)]
mod posix;
mod remux;
mod startup;
mod stream;
#[cfg(test)]
mod test_support;
mod transport;
mod update;
#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "mac.rs")]
mod win;
mod ytdlp;

fn main() -> eframe::Result {
    startup::start()
}
