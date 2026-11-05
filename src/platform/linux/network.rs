use super::{cmd, config_dir, Mutex, Stdio};

pub(super) fn gsettings(args: &[&str]) -> Option<String> {
    let o = cmd("gsettings")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .trim_matches('\'')
            .to_string(),
    )
}

/// The desktop's proxy as Windows' Internet Settings would give it: (on, "http=host:port;https=host:port",
/// bypass list). GNOME's settings (also Cinnamon, MATE, Budgie), else the environment (http_proxy,
/// https_proxy, ftp_proxy, no_proxy).
pub fn system_proxy() -> (bool, String, String) {
    if is_kde() {
        return kde_proxy(
            &std::fs::read_to_string(config_dir().join("kioslaverc")).unwrap_or_default(),
        );
    }
    match gsettings(&["get", "org.gnome.system.proxy", "mode"]).as_deref() {
        Some("manual") => {
            let hp = |k: &str| {
                let schema = format!("org.gnome.system.proxy.{k}");
                let host = gsettings(&["get", &schema, "host"]).unwrap_or_default();
                let port = gsettings(&["get", &schema, "port"]).unwrap_or_default();
                (!host.is_empty() && port != "0").then(|| format!("{k}={host}:{port}"))
            };
            let server = [hp("http"), hp("https"), hp("ftp")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(";");
            // ['localhost', '127.0.0.0/8']
            let ignore =
                gsettings(&["get", "org.gnome.system.proxy", "ignore-hosts"]).unwrap_or_default();
            let bypass = ignore
                .split(',')
                .map(|h| h.trim().trim_matches(['[', ']', '\'', ' ']))
                .filter(|h| !h.is_empty())
                .collect::<Vec<_>>()
                .join(";");
            return (!server.is_empty(), server, bypass);
        }
        Some("none") => return (false, String::new(), String::new()),
        _ => {}
    }
    let var = |k: &str| {
        std::env::var(k)
            .or_else(|_| std::env::var(k.to_uppercase()))
            .unwrap_or_default()
    };
    let host = |v: String| {
        v.split("://")
            .last()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string()
    };
    let (http, https, ftp) = (
        host(var("http_proxy")),
        host(var("https_proxy")),
        host(var("ftp_proxy")),
    );
    let server = [("http", http), ("https", https), ("ftp", ftp)]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    (
        !server.is_empty(),
        server,
        var("no_proxy").replace(',', ";"),
    )
}

fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_uppercase().contains("KDE"))
}

/// KDE's proxy (~/.config/kioslaverc) in `system_proxy`'s shape. Only ProxyType=1 (manual) counts as on;
/// a host is written "http://host port" or "host:port".
fn kde_proxy(text: &str) -> (bool, String, String) {
    let mut in_proxy = false;
    let (mut on, mut server, mut bypass) = (false, vec![], String::new());
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            in_proxy = l == "[Proxy Settings]";
            continue;
        }
        let Some((k, v)) = l.split_once('=').filter(|_| in_proxy) else {
            continue;
        };
        let hp = |v: &str| {
            let v = v
                .trim()
                .split("://")
                .last()
                .unwrap_or("")
                .trim_end_matches('/');
            v.rsplit_once(' ')
                .map_or(v.to_string(), |(h, p)| format!("{}:{p}", h.trim()))
        };
        match k.trim() {
            "ProxyType" => on = v.trim() == "1",
            "httpProxy" | "httpsProxy" | "ftpProxy" if !v.trim().is_empty() => {
                server.push(format!("{}={}", k.trim().trim_end_matches("Proxy"), hp(v)))
            }
            "NoProxyFor" => {
                bypass = v
                    .split(',')
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .collect::<Vec<_>>()
                    .join(";")
            }
            _ => {}
        }
    }
    (on && !server.is_empty(), server.join(";"), bypass)
}

#[test]
fn kde_proxy_reads_kioslaverc() {
    let t = "[General]\nhttpProxy=x\n[Proxy Settings]\nProxyType=1\nhttpProxy=http://10.0.0.1 3128\nhttpsProxy=10.0.0.2:3129\nNoProxyFor=localhost,.corp\n";
    assert_eq!(
        kde_proxy(t),
        (
            true,
            "http=10.0.0.1:3128;https=10.0.0.2:3129".into(),
            "localhost;.corp".into()
        )
    );
    assert!(!kde_proxy(&t.replace("ProxyType=1", "ProxyType=2")).0);
}

/// Writes KDE's proxy (kioslaverc, through kwriteconfig6 or 5) and tells running KDE programs.
fn set_kde_proxy(on: bool, server: &str, bypass: &str) {
    let write = |key: &str, v: &str| {
        let args = [
            "--file",
            "kioslaverc",
            "--group",
            "Proxy Settings",
            "--key",
            key,
            v,
        ];
        let ok = |p: &str| {
            cmd(p)
                .args(args)
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        let _ = ok("kwriteconfig6") || ok("kwriteconfig5");
    };
    if on {
        for kind in ["http", "https", "ftp"] {
            let hp = server
                .split(';')
                .find_map(|p| p.strip_prefix(&format!("{kind}=")))
                .or_else(|| (!server.contains('=')).then_some(server))
                .unwrap_or("");
            write(
                &format!("{kind}Proxy"),
                &if hp.is_empty() {
                    String::new()
                } else {
                    format!("http://{hp}")
                },
            );
        }
        write(
            "NoProxyFor",
            &bypass
                .split(';')
                .map(str::trim)
                .filter(|h| !h.is_empty() && *h != "<local>")
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    write("ProxyType", if on { "1" } else { "0" });
    let _ = cmd("dbus-send")
        .args([
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
            "string:",
        ])
        .stderr(Stdio::null())
        .status();
}

/// Sets the desktop's proxy (KDE's or GNOME's settings, as `system_proxy` reads them) for the browsers.
/// `server` is "host:port" or "http=host:port;https=host:port".
pub fn set_system_proxy(on: bool, server: &str, bypass: &str) {
    if is_kde() {
        return set_kde_proxy(on, server, bypass);
    }
    let set = |schema: &str, key: &str, v: &str| {
        let _ = cmd("gsettings")
            .args(["set", schema, key, v])
            .stderr(Stdio::null())
            .status();
    };
    if !on {
        set("org.gnome.system.proxy", "mode", "none");
        return;
    }
    for kind in ["http", "https"] {
        let hp = server
            .split(';')
            .find_map(|p| p.strip_prefix(&format!("{kind}=")))
            .or_else(|| (!server.contains('=')).then_some(server))
            .unwrap_or("");
        let (host, port) = hp.rsplit_once(':').unwrap_or((hp, "0"));
        let schema = format!("org.gnome.system.proxy.{kind}");
        set(&schema, "host", host);
        set(&schema, "port", port);
    }
    let hosts: Vec<String> = bypass
        .split(';')
        .map(str::trim)
        .filter(|h| !h.is_empty() && *h != "<local>")
        .map(|h| format!("'{h}'"))
        .collect();
    set(
        "org.gnome.system.proxy",
        "ignore-hosts",
        &format!("[{}]", hosts.join(", ")),
    );
    set("org.gnome.system.proxy", "mode", "manual");
}

/// Connection types that dial: VPNs, WireGuard, PPPoE and mobile broadband.
const DIAL_TYPES: [&str; 5] = ["vpn", "wireguard", "pppoe", "gsm", "cdma"];

/// Dial connections as nmcli lists them ("name:type", a ':' in a name as "\:"): (name, type).
/// None = no NetworkManager.
fn nm_conns(active: bool) -> Option<Vec<(String, String)>> {
    let mut c = cmd("nmcli");
    c.args(["-t", "-f", "NAME,TYPE", "connection", "show"]);
    if active {
        c.arg("--active");
    }
    let o = c
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(nm_parse(&String::from_utf8_lossy(&o.stdout)))
}

fn nm_parse(text: &str) -> Vec<(String, String)> {
    // types: "vpn", "wireguard", "pppoe", "gsm", "802-11-wireless", ...
    text.lines()
        .filter_map(|l| l.rsplit_once(':'))
        .filter(|(_, t)| DIAL_TYPES.contains(t))
        .map(|(n, t)| (n.replace("\\:", ":"), t.to_string()))
        .collect()
}

#[test]
fn nmcli_lines() {
    let text = "Home Wi-Fi:802-11-wireless\nOffice\\: VPN:vpn\nwg0:wireguard\nlo:loopback\n";
    assert_eq!(
        nm_parse(text),
        [
            ("Office: VPN".to_string(), "vpn".to_string()),
            ("wg0".into(), "wireguard".into())
        ]
    );
}

/// NetworkManager's dial-up / VPN connections.
pub fn ras_entries() -> Vec<String> {
    nm_conns(false)
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.0)
        .collect()
}

/// Is a dial-up / VPN connection up? Without NetworkManager there is nothing to dial: yes.
pub fn ras_connected() -> bool {
    nm_conns(true).is_none_or(|c| !c.is_empty())
}

/// Passwords for this session only ("Save password" unticked), handed to nmcli when dialing.
static PASSWORDS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn nm_kind(entry: &str) -> Option<String> {
    nm_conns(false)?
        .into_iter()
        .find(|c| c.0 == entry)
        .map(|c| c.1)
}

fn nmcli(args: &[&str]) -> Result<(), String> {
    let o = cmd("nmcli")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr)
            .trim()
            .trim_start_matches("Error: ")
            .to_string())
    }
}

/// User name (and, with `save`, the password) NetworkManager keeps for `entry` (Dial Up page's Apply).
pub fn ras_save(entry: &str, user: &str, pass: &str, save: bool) -> Result<(), String> {
    let kind = nm_kind(entry).ok_or("NetworkManager has no such connection")?;
    PASSWORDS.lock().unwrap().retain(|p| p.0 != entry);
    if !save && !pass.is_empty() {
        PASSWORDS
            .lock()
            .unwrap()
            .push((entry.to_string(), pass.to_string()));
    }
    let pass = if save { pass } else { "" };
    match kind.as_str() {
        "wireguard" => Ok(()), // keys, no login
        "pppoe" | "gsm" | "cdma" => nmcli(&[
            "connection",
            "modify",
            "id",
            entry,
            &format!("{kind}.username"),
            user,
            &format!("{kind}.password"),
            pass,
        ]),
        _ => {
            nmcli(&["connection", "modify", "id", entry, "vpn.user-name", user])?;
            if save {
                nmcli(&[
                    "connection",
                    "modify",
                    "id",
                    entry,
                    "+vpn.data",
                    "password-flags=0",
                    "+vpn.secrets",
                    &format!("password={pass}"),
                ])
            } else {
                Ok(())
            }
        }
    }
}

/// Brings `entry` up (synchronously), with this session's password when one was given.
pub fn ras_dial(entry: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let pass = PASSWORDS
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.0 == entry)
        .map(|p| p.1.clone());
    let file = std::env::temp_dir().join(format!("ndm-dial-{}", std::process::id()));
    let mut args = vec!["--wait", "90", "connection", "up", "id", entry];
    if let Some(p) = &pass {
        let setting = match nm_kind(entry).as_deref() {
            Some("pppoe") => "pppoe.password",
            Some("gsm") => "gsm.password",
            Some("cdma") => "cdma.password",
            _ => "vpn.secrets.password",
        };
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{setting}:{p}").map_err(|e| e.to_string())?;
        args.extend(["passwd-file", file.to_str().unwrap_or("")]);
    }
    let r = nmcli(&args);
    let _ = std::fs::remove_file(&file);
    r
}

/// "Hang up modem when done": every dial-up / VPN connection that is up.
pub fn ras_hangup_all() {
    for (name, _) in nm_conns(true).unwrap_or_default() {
        let _ = nmcli(&["connection", "down", "id", &name]);
    }
}

/// The networks a download can be spread over: up, not loopback, with an address and a default route.
pub fn nics() -> Vec<crate::net::Nic> {
    let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let route6 = std::fs::read_to_string("/proc/net/ipv6_route").unwrap_or_default();
    crate::posix::ifaddrs(&default_route_devices(&route, &route6))
}

/// The devices with a default route in /proc/net/route (Destination and Mask 00000000) and
/// /proc/net/ipv6_route (all-zero destination with prefix length 00).
fn default_route_devices(route: &str, route6: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut add = |d: &str| {
        if d != "lo" && !out.iter().any(|o| o == d) {
            out.push(d.to_string());
        }
    };
    for l in route.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() > 7 && f[1] == "00000000" && f[7] == "00000000" {
            add(f[0]);
        }
    }
    for l in route6.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() == 10 && f[0].bytes().all(|b| b == b'0') && f[1] == "00" {
            add(f[9]);
        }
    }
    out
}

#[test]
fn default_routes() {
    let route =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        wlp2s0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
        wlp2s0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
        docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";
    let route6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 enx0a1b2c3d4e5f\n\
        00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n\
        fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 wlp2s0\n";
    assert_eq!(
        default_route_devices(route, route6),
        ["wlp2s0", "enx0a1b2c3d4e5f"]
    );
}
