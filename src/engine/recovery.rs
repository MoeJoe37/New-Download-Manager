//! Recovery choices derived from the existing saved error text. Download records keep their format.
use super::{CHANGED, Info, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    Retry,
    RefreshAddress,
    Credentials,
    ProxySettings,
    MediaTools,
    Restart,
    OpenFolder,
}

impl RecoveryAction {
    pub(crate) fn for_download(info: &Info) -> Self {
        if info.fed {
            return Self::RefreshAddress;
        }
        let Status::Error(error) = &info.status else { return Self::Retry };
        let lower = error.to_ascii_lowercase();
        if error == CHANGED || lower.contains("saved download ranges are invalid")
            || lower.contains("partial download is shorter") {
            return Self::Restart;
        }
        if has_status(&lower, "407") || lower.contains("proxy authentication") {
            return Self::ProxySettings;
        }
        if has_status(&lower, "401") || lower.contains("login required") || lower.contains("authentication required") {
            return Self::Credentials;
        }
        if lower.contains("set yt-dlp") || lower.contains("requires ffmpeg") || lower.contains("needs ffmpeg")
            || lower.contains("could not join video and audio") {
            return Self::MediaTools;
        }
        if ["403", "404", "410"].iter().any(|code| has_status(&lower, code)) || lower.contains("expired") {
            return Self::RefreshAddress;
        }
        if ["no space left", "disk full", "permission denied", "access is denied", "read-only file system"]
            .iter().any(|text| lower.contains(text)) {
            return Self::OpenFolder;
        }
        Self::Retry
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Retry => "Retry",
            Self::RefreshAddress => "Refresh download address",
            Self::Credentials => "Edit credentials",
            Self::ProxySettings => "Proxy / Socks",
            Self::MediaTools => "Install media tools",
            Self::Restart => "Restart download",
            Self::OpenFolder => "Open folder",
        }
    }

    pub(crate) fn guidance(self) -> &'static str {
        match self {
            Self::Retry => "Retry this download with its current settings.",
            Self::RefreshAddress => "Open the original page and capture a new address for this download. Its selected quality and partial data are kept.",
            Self::Credentials => "Update the login details in File Properties, then retry the download.",
            Self::ProxySettings => "Check the proxy address and credentials in Options, then retry the download.",
            Self::MediaTools => "Install or update yt-dlp, Deno and FFmpeg, then retry the download. Downloaded streams are kept.",
            Self::Restart => "The server file or saved ranges changed. Restarting replaces the partial download.",
            Self::OpenFolder => "Check space and permissions in the destination folder, then retry the download.",
        }
    }
}

fn has_status(message: &str, code: &str) -> bool {
    message.split_ascii_whitespace().any(|word| word.trim_matches(['[', ']', '(', ')', ':', ',']) == code)
}

#[test]
fn recovery_keeps_identity_changes_separate_from_retry_and_proxy_authentication() {
    let mut info = Info::default();
    for (message, action) in [
        (CHANGED, RecoveryAction::Restart),
        ("Saved download ranges are invalid. Restart the download to recover.", RecoveryAction::Restart),
        ("HTTP 407: Proxy Authentication Required", RecoveryAction::ProxySettings),
        ("HTTP 401: authentication required", RecoveryAction::Credentials),
        ("This audio format requires FFmpeg. Install it with Downloads -> Set yt-dlp and resume.", RecoveryAction::MediaTools),
        ("HTTP 403: signature expired", RecoveryAction::RefreshAddress),
        ("No space left on device", RecoveryAction::OpenFolder),
        ("Permission denied: /downloads/video401.mp4", RecoveryAction::OpenFolder),
        ("Connection failed at http://localhost:407/file", RecoveryAction::Retry),
        ("Connection closed early", RecoveryAction::Retry),
    ] {
        info.status = Status::Error(message.into());
        assert_eq!(RecoveryAction::for_download(&info), action, "{message}");
    }
    info.fed = true;
    assert_eq!(RecoveryAction::for_download(&info), RecoveryAction::RefreshAddress);
}
