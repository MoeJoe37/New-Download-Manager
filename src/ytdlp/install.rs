use crate::ytdlp::{command, exe_file, ffmpeg, runs, AtomicU64, Path, PathBuf, Read, Relaxed};

/// The newest releases' files: GitHub sends ".../releases/latest/download/<file>" on to the newest one.
const YTDLP_LATEST: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

const DENO_LATEST: &str = "https://github.com/denoland/deno/releases/latest/download";

/// The folder of NDM's own yt-dlp and Deno, beside its settings (added to PATH by `install`).
pub fn tools_dir() -> PathBuf {
    crate::core::state::store_path()
        .parent()
        .map(|d| d.join("tools"))
        .unwrap_or_else(|| PathBuf::from("tools"))
}

/// yt-dlp's release file for this system: its stand-alone builds, which need no Python.
pub(super) fn ytdlp_asset() -> &'static str {
    if cfg!(windows) {
        if cfg!(target_arch = "aarch64") {
            "yt-dlp_arm64.exe"
        } else if cfg!(target_arch = "x86") {
            "yt-dlp_x86.exe"
        } else {
            "yt-dlp.exe"
        }
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

/// Deno's release file for this system.
pub(super) fn deno_asset() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let os = if cfg!(windows) {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown-linux-gnu"
    };
    format!("deno-{arch}-{os}.zip")
}

/// The SHA-256 a sums file lists for `file` ("hex  name" lines; Deno's .sha256sum has one line).
pub(super) fn listed_sum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hex, name) = l.split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name.rsplit(['/', '\\']).next() == Some(file)
            && hex.len() == 64
            && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
    })
}

/// The one SHA-256 in a file that has only one: "hex  name" (Linux, macOS) or PowerShell's
/// "Algorithm : SHA256 / Hash : HEX / Path : ..." (Deno's Windows releases).
pub(super) fn only_sum(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .find(|t| t.len() == 64)
        .map(str::to_ascii_lowercase)
}

fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent
        .get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())
}

/// Downloads `url` to `out`, checked against SHA-256 `want` (a damaged file is removed). `got` / `total`
/// add up over every file of the install.
fn get_checked(
    agent: &ureq::Agent,
    url: &str,
    want: &str,
    out: &Path,
    got: &AtomicU64,
    total: &AtomicU64,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    total.fetch_add(
        r.header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        Relaxed,
    );
    let mut file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let (mut body, mut hash, mut buf) = (r.into_reader(), Sha256::new(), vec![0u8; 1 << 16]);
    loop {
        let n = body.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got.fetch_add(n as u64, Relaxed);
    }
    drop(file);
    let sum = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if sum != want {
        let _ = std::fs::remove_file(out);
        let name = url.rsplit('/').next().unwrap_or(url);
        return Err(format!(
            "{name}: the downloaded file is damaged (SHA-256 {sum}, expected {want})"
        ));
    }
    Ok(())
}

/// Puts the new program `part` in place of `dest`, runnable. Windows can't overwrite a program that runs
/// (a video downloading), but can rename it out of the way.
fn put_in_place(part: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(part, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    let old = dest.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(part, dest).is_err() {
        std::fs::rename(dest, &old).map_err(|e| e.to_string())?;
        std::fs::rename(part, dest).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The one file `name` (at any depth) from a zip archive's bytes: stored or deflated, zip64 offsets too.
pub(super) fn unzip_one(zip: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let u16le = |at: usize| {
        zip.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let u32le = |at: usize| {
        zip.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
    };
    let u64le = |at: usize| {
        zip.get(at..at + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    let bad = || "not a zip archive".to_string();
    let eocd = (0..zip.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| zip[i..i + 4] == [0x50, 0x4b, 5, 6])
        .ok_or_else(bad)?;
    let (count, mut at) = (
        u16le(eocd + 10).ok_or_else(bad)?,
        u32le(eocd + 16).ok_or_else(bad)? as usize,
    );
    // zip64: the central directory's place in the zip64 end record
    if at == 0xFFFF_FFFF && eocd >= 20 && zip[eocd - 20..eocd - 16] == [0x50, 0x4b, 6, 7] {
        let rec = u64le(eocd - 12).ok_or_else(bad)? as usize;
        at = u64le(rec + 48).ok_or_else(bad)? as usize;
    }
    for _ in 0..count.max(1) {
        if zip.get(at..at + 4) != Some(&[0x50, 0x4b, 1, 2]) {
            break;
        }
        let (method, nlen, xlen, clen) = (
            u16le(at + 10).ok_or_else(bad)?,
            u16le(at + 28).ok_or_else(bad)?,
            u16le(at + 30).ok_or_else(bad)?,
            u16le(at + 32).ok_or_else(bad)?,
        );
        let (mut csize, mut usize_, mut local) = (
            u32le(at + 20).ok_or_else(bad)?,
            u32le(at + 24).ok_or_else(bad)?,
            u32le(at + 42).ok_or_else(bad)?,
        );
        let entry =
            String::from_utf8_lossy(zip.get(at + 46..at + 46 + nlen).ok_or_else(bad)?).to_string();
        // zip64 sizes and offset, in that order, for the fields that are 0xFFFFFFFF
        let mut x = at + 46 + nlen;
        while x + 4 <= at + 46 + nlen + xlen {
            let (id, len) = (u16le(x).ok_or_else(bad)?, u16le(x + 2).ok_or_else(bad)?);
            if id == 1 {
                let mut v = x + 4;
                for f in [&mut usize_, &mut csize, &mut local] {
                    if *f == 0xFFFF_FFFF {
                        *f = u64le(v).ok_or_else(bad)?;
                        v += 8;
                    }
                }
            }
            x += 4 + len;
        }
        at += 46 + nlen + xlen + clen;
        if entry.rsplit('/').next() != Some(name) {
            continue;
        }
        let local = local as usize;
        let data =
            local + 30 + u16le(local + 26).ok_or_else(bad)? + u16le(local + 28).ok_or_else(bad)?;
        let raw = zip.get(data..data + csize as usize).ok_or_else(bad)?;
        return match method {
            0 => Ok(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(usize_ as usize);
                flate2::read::DeflateDecoder::new(raw)
                    .read_to_end(&mut out)
                    .map_err(|e| e.to_string())?;
                Ok(out)
            }
            m => Err(format!("{name}: zip method {m} isn't supported")),
        };
    }
    Err(format!("{name} is not in the archive"))
}

/// The SHA-256 of the release file NDM's copy came from (written next to it).
fn installed_sum(dir: &Path, prog: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{prog}.sha256")))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// yt-dlp into `dir` from `base` (GitHub's newest release; a local stand-in in the tests), when the
/// release's file differs from the one NDM's copy came from or the copy doesn't run.
pub(super) fn fetch_ytdlp(
    agent: &ureq::Agent,
    base: &str,
    dir: &Path,
    got: &AtomicU64,
    total: &AtomicU64,
) -> Result<(), String> {
    let file = ytdlp_asset();
    let want = listed_sum(&get_text(agent, &format!("{base}/SHA2-256SUMS"))?, file)
        .ok_or_else(|| format!("{file} is not in yt-dlp's release"))?;
    let dest = dir.join(exe_file("yt-dlp"));
    if installed_sum(dir, "yt-dlp") == want && runs(&dest) {
        return Ok(());
    }
    let part = dir.join(format!("{}.part", exe_file("yt-dlp")));
    get_checked(agent, &format!("{base}/{file}"), &want, &part, got, total)?;
    put_in_place(&part, &dest)?;
    let _ = std::fs::write(dir.join("yt-dlp.sha256"), &want);
    Ok(())
}

/// Deno into `dir` from `base`, the same way (its release is a zip with the one program).
pub(super) fn fetch_deno(
    agent: &ureq::Agent,
    base: &str,
    dir: &Path,
    got: &AtomicU64,
    total: &AtomicU64,
) -> Result<(), String> {
    let zip = deno_asset();
    let want = only_sum(&get_text(agent, &format!("{base}/{zip}.sha256sum"))?)
        .ok_or_else(|| format!("{zip}: no SHA-256 in its release"))?;
    let dest = dir.join(exe_file("deno"));
    if installed_sum(dir, "deno") == want && runs(&dest) {
        return Ok(());
    }
    let archive = dir.join(format!("{zip}.part"));
    get_checked(agent, &format!("{base}/{zip}"), &want, &archive, got, total)?;
    let bytes = std::fs::read(&archive).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&archive);
    let program = unzip_one(&bytes?, &exe_file("deno"))?;
    let part = dir.join(format!("{}.part", exe_file("deno")));
    std::fs::write(&part, program).map_err(|e| e.to_string())?;
    put_in_place(&part, &dest)?;
    let _ = std::fs::write(dir.join("deno.sha256"), &want);
    Ok(())
}

/// ffmpeg's release file for this system: yt-dlp's own ffmpeg builds (Windows, Linux; SHA-256s in their
/// checksums.sha256), martin-riedl.de's for macOS (a .sha256 beside each build's file). None: no build for it.
fn ffmpeg_asset() -> Option<(&'static str, &'static str)> {
    const YTDLP_FFMPEG: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/latest/download";
    Some(if cfg!(windows) {
        if cfg!(target_arch = "aarch64") {
            (YTDLP_FFMPEG, "ffmpeg-master-latest-winarm64-gpl.zip")
        } else if cfg!(target_arch = "x86_64") {
            (YTDLP_FFMPEG, "ffmpeg-master-latest-win64-gpl.zip")
        } else {
            return None;
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            (
                "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release",
                "ffmpeg.zip",
            )
        } else {
            (
                "https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release",
                "ffmpeg.zip",
            )
        }
    } else if cfg!(target_arch = "aarch64") {
        (YTDLP_FFMPEG, "ffmpeg-master-latest-linuxarm64-gpl.tar.xz")
    } else if cfg!(target_arch = "x86_64") {
        (YTDLP_FFMPEG, "ffmpeg-master-latest-linux64-gpl.tar.xz")
    } else {
        return None;
    })
}

/// The programs `names` out of a .tar stream (ffmpeg's Linux builds, after xz), each written to
/// `dir/<name>.part`; the rest is read past. Returns how many were found.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn untar_some(mut r: impl Read, names: &[&str], dir: &Path) -> Result<usize, String> {
    let mut found = 0;
    let mut head = [0u8; 512];
    loop {
        if r.read_exact(&mut head).is_err() || head.iter().all(|b| *b == 0) {
            return Ok(found);
        }
        let field = |a: usize, b: usize| {
            String::from_utf8_lossy(&head[a..b])
                .trim_end_matches('\0')
                .to_string()
        };
        let octal = field(124, 136);
        let size = u64::from_str_radix(octal.trim().trim_matches('\0'), 8)
            .map_err(|_| "not a tar archive".to_string())?;
        let prefix = field(345, 500);
        let path = if head[257..262] == *b"ustar" && !prefix.is_empty() {
            format!("{prefix}/{}", field(0, 100))
        } else {
            field(0, 100)
        };
        let file = path.rsplit('/').next().unwrap_or("").to_string();
        let mut body = (&mut r).take(size.div_ceil(512) * 512);
        if matches!(head[156], b'0' | 0) && names.contains(&file.as_str()) {
            let mut out = std::fs::File::create(dir.join(format!("{file}.part")))
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut (&mut body).take(size), &mut out).map_err(|e| e.to_string())?;
            found += 1;
        }
        std::io::copy(&mut body, &mut std::io::sink()).map_err(|e| e.to_string())?;
    }
}

/// ffmpeg (and ffprobe, which yt-dlp asks for alongside it) into `dir`, checked against its SHA-256, when
/// no ffmpeg runs yet: NDM's own copy, one beside NDM or one on PATH. Its builds are made every day, so a
/// working copy stays as it is rather than coming down again each day. yt-dlp then joins video and audio
/// itself, in any format.
pub(super) fn fetch_ffmpeg(
    agent: &ureq::Agent,
    dir: &Path,
    got: &AtomicU64,
    total: &AtomicU64,
) -> Result<(), String> {
    if ffmpeg().is_some() {
        return Ok(());
    }
    let Some((base, file)) = ffmpeg_asset() else {
        return Err("ffmpeg: no build for this system".into());
    };
    if base.contains("yt-dlp") {
        let want = listed_sum(&get_text(agent, &format!("{base}/checksums.sha256"))?, file)
            .ok_or_else(|| format!("{file}: no SHA-256 in its release"))?;
        let archive = dir.join(format!("{file}.part"));
        get_checked(
            agent,
            &format!("{base}/{file}"),
            &want,
            &archive,
            got,
            total,
        )?;
        let result = unpack_ffmpeg(&archive, file, dir);
        let _ = std::fs::remove_file(&archive);
        return result;
    }
    // macOS: ffmpeg and ffprobe are zips of their own; "latest" sends on to the build's folder, where each
    // has its .sha256 beside it
    for zip in ["ffmpeg.zip", "ffprobe.zip"] {
        let fetched = (|| -> Result<(), String> {
            // its "latest" address answers GET only (HEAD: 404); the body isn't read here
            let url = agent
                .get(&format!("{base}/{zip}"))
                .call()
                .map_err(|e| format!("{base}/{zip}: {e}"))?
                .get_url()
                .to_string();
            let want = only_sum(&get_text(agent, &format!("{url}.sha256"))?)
                .ok_or_else(|| format!("{zip}: no SHA-256 beside it"))?;
            let archive = dir.join(format!("{zip}.part"));
            get_checked(agent, &url, &want, &archive, got, total)?;
            let result = unpack_ffmpeg(&archive, zip, dir);
            let _ = std::fs::remove_file(&archive);
            result
        })();
        // ffprobe is a help to yt-dlp, not a need
        if zip == "ffmpeg.zip" {
            fetched?;
        } else if let Err(e) = fetched {
            eprintln!("ffprobe: {e}");
        }
    }
    Ok(())
}

/// ffmpeg and ffprobe out of the downloaded `archive` (release file `file`: .zip or .tar.xz) into `dir`,
/// in place of older copies; ffmpeg must start.
pub(super) fn unpack_ffmpeg(archive: &Path, file: &str, dir: &Path) -> Result<(), String> {
    let progs = [exe_file("ffmpeg"), exe_file("ffprobe")];
    let result = (|| -> Result<(), String> {
        if file.ends_with(".tar.xz") {
            #[cfg(target_os = "linux")]
            {
                let xz = xz2::read::XzDecoder::new(std::io::BufReader::new(
                    std::fs::File::open(archive).map_err(|e| e.to_string())?,
                ));
                untar_some(
                    xz,
                    &progs.iter().map(String::as_str).collect::<Vec<_>>(),
                    dir,
                )?;
            }
        } else {
            let bytes = std::fs::read(archive).map_err(|e| e.to_string())?;
            for p in &progs {
                // macOS: ffprobe is a release of its own; ffmpeg is what matters
                if let Ok(program) = unzip_one(&bytes, p) {
                    std::fs::write(dir.join(format!("{p}.part")), program)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        // the archive's own program (macOS has one zip each), else ffmpeg
        let need = if file.starts_with("ffprobe") {
            &progs[1]
        } else {
            &progs[0]
        };
        if !dir.join(format!("{need}.part")).is_file() {
            return Err(format!("{file}: no {need} in it"));
        }
        for p in &progs {
            let part = dir.join(format!("{p}.part"));
            if part.is_file() {
                put_in_place(&part, &dir.join(p))?;
            }
        }
        Ok(())
    })();
    for p in &progs {
        let _ = std::fs::remove_file(dir.join(format!("{p}.part")));
    }
    result?;
    if !file.starts_with("ffprobe") && !runs(&dir.join(&progs[0])) {
        return Err("ffmpeg was downloaded but doesn't start".into());
    }
    Ok(())
}

/// Downloads (or brings up to date) yt-dlp and Deno in NDM's tools folder, each checked against the SHA-256
/// its release lists, and (`with_ffmpeg`: Downloads -> Set yt-dlp, asked for) ffmpeg when there is none yet;
/// makes sure the folder is on PATH. The daily update leaves ffmpeg out: it is 100+ MB, and comes only when
/// the user asks for it. `got` / `total`: progress for the window. Returns yt-dlp's version.
pub fn install(
    agent: &ureq::Agent,
    got: &AtomicU64,
    total: &AtomicU64,
    with_ffmpeg: bool,
) -> Result<String, String> {
    let dir = tools_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fetch_ytdlp(agent, YTDLP_LATEST, &dir, got, total)?;
    fetch_deno(agent, DENO_LATEST, &dir, got, total)?;
    // yt-dlp works without ffmpeg (NDM joins the usual formats itself): it is on PATH before ffmpeg's turn
    add_to_path(&dir);
    if with_ffmpeg {
        fetch_ffmpeg(agent, &dir, got, total)?;
        add_to_path(&dir);
    }
    Ok(version(&dir.join(exe_file("yt-dlp"))))
}

pub(super) fn version(p: &Path) -> String {
    command(p)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_default()
}

/// Puts NDM's tools folder on the user's PATH, so yt-dlp works in a terminal too: the user's Path in the
/// registry on Windows, links in ~/.local/bin on Linux, a line in ~/.zprofile on macOS.
fn add_to_path(dir: &Path) {
    #[cfg(windows)]
    crate::win::add_user_path(dir);
    #[cfg(target_os = "linux")]
    if let Some(home) = std::env::var_os("HOME") {
        let bin = Path::new(&home).join(".local/bin");
        let _ = std::fs::create_dir_all(&bin);
        for prog in ["yt-dlp", "deno", "ffmpeg", "ffprobe"] {
            if !dir.join(prog).is_file() {
                continue;
            }
            let link = bin.join(prog);
            // a program of the user's own stays
            if std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(&link);
            }
            let _ = std::os::unix::fs::symlink(dir.join(prog), &link);
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        let profile = Path::new(&home).join(".zprofile");
        let text = std::fs::read_to_string(&profile).unwrap_or_default();
        let line = format!("export PATH=\"$PATH:{}\"", dir.display());
        if !text.lines().any(|l| l.trim() == line) {
            let sep = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            let _ = std::fs::write(
                &profile,
                format!("{text}{sep}# New Download Manager's yt-dlp\n{line}\n"),
            );
        }
    }
}

/// Keeps NDM's copies up to date: once a day, when there is one (video sites change often and an old
/// yt-dlp stops working on them). Only the small checksum lists are fetched unless a release changed.
pub fn keep_updated(core: std::sync::Arc<crate::core::Core>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        loop {
            let now = chrono::Local::now().timestamp();
            let due = now - core.settings().ytdlp_checked > 86400;
            if due && tools_dir().join(exe_file("yt-dlp")).is_file() {
                core.settings().ytdlp_checked = now;
                let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
                if let Err(e) = install(&core.agent(YTDLP_LATEST), &got, &total, false) {
                    eprintln!("yt-dlp update: {e}");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    });
}
