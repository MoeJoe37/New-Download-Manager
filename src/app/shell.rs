use super::*;

/// Virus scanner arguments: `[File]` replaced by the downloaded file, or the file appended (template 494's note).
pub(in crate::app) fn virus_args(args: &str, file: &Path) -> Vec<String> {
    let f = file.display().to_string();
    let mut v: Vec<String> = args.split_whitespace().map(|a| a.replace("[File]", &f)).collect();
    if !args.contains("[File]") {
        v.push(f);
    }
    v
}

#[test]
fn virus_arg_list() {
    let p = Path::new(r"C:\d\a b.zip");
    assert_eq!(virus_args("", p), [r"C:\d\a b.zip"]);
    assert_eq!(virus_args("/scan=[File] /q", p), [r"/scan=C:\d\a b.zip", "/q"]);
}

pub(in crate::app) fn explore(arg: impl AsRef<std::ffi::OsStr>) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(arg).spawn();
    #[cfg(not(windows))]
    win::open(arg);
}

/// The file's folder with the file selected (Explorer, Finder, the Linux file manager); its folder when the
/// file isn't there (yet).
pub(in crate::app) fn open_folder_of(path: &Path) {
    if path.is_file() {
        win::show_in_folder(path);
    } else if let Some(p) = path.ancestors().skip(1).find(|p| p.is_dir()) {
        explore(p);
    }
}

/// File-type filters offered by the wizard's drop-down lists.
/// Open a web address in the default browser.
pub(in crate::app) fn open_url(u: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", u]).spawn();
    #[cfg(not(windows))]
    win::open(u);
}

/// "Open with...": the system's program chooser.
pub(in crate::app) fn open_with(p: impl AsRef<std::ffi::OsStr>) {
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").arg("shell32.dll,OpenAs_RunDLL").arg(p).spawn();
    #[cfg(not(windows))]
    win::open_with(Path::new(p.as_ref()));
}
