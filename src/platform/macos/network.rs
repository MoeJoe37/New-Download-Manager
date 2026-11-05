use super::output;

/// The system's proxy (System Settings -> Network -> Proxies, as `scutil --proxy` shows it) in Windows'
/// Internet Settings shape: (on, "http=host:port;https=host:port", bypass list).
pub fn system_proxy() -> (bool, String, String) {
    let (on, mut server, bypass) =
        scutil_proxy(&output("scutil", &["--proxy"]).unwrap_or_default());
    if on && crate::mac_capture::has_journal() {
        let local = format!("http=127.0.0.1:{}", crate::core::routing::PROXY_PORT);
        server = server
            .split(';')
            .filter(|entry| *entry != local)
            .collect::<Vec<_>>()
            .join(";");
    }
    (!server.is_empty(), server, bypass)
}

fn scutil_proxy(text: &str) -> (bool, String, String) {
    let (mut keys, mut bypass, mut in_list) = (std::collections::HashMap::new(), vec![], false);
    for l in text.lines().map(str::trim) {
        if l.starts_with("ExceptionsList") {
            in_list = true;
            continue;
        }
        if in_list {
            match l.split_once(" : ") {
                Some((_, v)) => bypass.push(v.trim().to_string()),
                None => in_list = false,
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(" : ") {
            keys.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let get = |k: &str| keys.get(k).cloned().unwrap_or_default();
    let server: Vec<String> = [("http", "HTTP"), ("https", "HTTPS"), ("ftp", "FTP")]
        .iter()
        .filter(|(_, k)| get(&format!("{k}Enable")) == "1" && !get(&format!("{k}Proxy")).is_empty())
        .map(|(n, k)| {
            format!(
                "{n}={}:{}",
                get(&format!("{k}Proxy")),
                get(&format!("{k}Port"))
            )
        })
        .collect();
    (!server.is_empty(), server.join(";"), bypass.join(";"))
}

#[test]
fn scutil_proxy_output() {
    let t = "<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n    1 : 169.254/16\n  }\n  FTPPassive : 1\n  HTTPEnable : 1\n  HTTPPort : 3128\n  HTTPProxy : 10.0.0.1\n  HTTPSEnable : 0\n  HTTPSPort : 3129\n  HTTPSProxy : 10.0.0.2\n}\n";
    assert_eq!(
        scutil_proxy(t),
        (
            true,
            "http=10.0.0.1:3128".into(),
            "*.local;169.254/16".into()
        )
    );
    assert!(!scutil_proxy("<dictionary> {\n  HTTPEnable : 0\n}\n").0);
}

/// (name, connected) for each VPN service `scutil --nc list` shows: `* (Connected)  ID  IPSec  "Work"  [IPSec]`.
fn nc_list() -> Vec<(String, bool)> {
    output("scutil", &["--nc", "list"])
        .map(|t| nc_parse(&t))
        .unwrap_or_default()
}

fn nc_parse(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let name = l.split('"').nth(1)?;
            Some((name.to_string(), l.contains("(Connected)")))
        })
        .collect()
}

#[test]
fn nc_lines() {
    let t = "Available network connection services in the current set (*=enabled):\n* (Disconnected)   A1B2  PPP --> L2TP  \"Office VPN\"   [PPP/L2TP]\n* (Connected)      C3D4  IPSec         \"Home\"         [IPSec]\n";
    assert_eq!(
        nc_parse(t),
        [
            ("Office VPN".to_string(), false),
            ("Home".to_string(), true)
        ]
    );
}

pub fn ras_entries() -> Vec<String> {
    nc_list().into_iter().map(|c| c.0).collect()
}

/// Is a VPN up? Without VPN services there is nothing to dial: yes.
pub fn ras_connected() -> bool {
    let l = nc_list();
    l.is_empty() || l.iter().any(|c| c.1)
}

/// macOS keeps a VPN's login in its own settings (System Settings -> VPN): nothing to save here.
pub fn ras_save(_entry: &str, _user: &str, _pass: &str, _save: bool) -> Result<(), String> {
    Ok(())
}

/// Starts `entry` and waits up to 90 s for it to connect.
pub fn ras_dial(entry: &str) -> Result<(), String> {
    output("scutil", &["--nc", "start", entry]).ok_or("the VPN didn't start")?;
    for _ in 0..180 {
        if output("scutil", &["--nc", "status", entry])
            .is_some_and(|s| s.lines().next().is_some_and(|l| l.trim() == "Connected"))
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("the VPN didn't connect".into())
}

pub fn ras_hangup_all() {
    for (name, up) in nc_list() {
        if up {
            let _ = output("scutil", &["--nc", "stop", &name]);
        }
    }
}

/// The networks a download can be spread over: up, not loopback, with an address and a default route
/// (`netstat -rn`, the scoped default routes included: one per connected network).
pub fn nics() -> Vec<crate::net::Nic> {
    let routes = [
        output("netstat", &["-rn", "-f", "inet"]),
        output("netstat", &["-rn", "-f", "inet6"]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    crate::posix::ifaddrs(&default_route_devices(&routes))
}

/// The interfaces of `netstat -rn`'s "default" lines (Destination Gateway Flags Netif [Expire]).
fn default_route_devices(netstat: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in netstat.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.first() == Some(&"default")
            && f.len() >= 4
            && !f[3].starts_with("lo")
            && !out.iter().any(|o| o == f[3])
        {
            out.push(f[3].to_string());
        }
    }
    out
}

#[test]
fn netstat_defaults() {
    let t = "Routing tables\n\nInternet:\nDestination        Gateway            Flags               Netif Expire\ndefault            192.168.1.1        UGScg                 en0       \ndefault            172.20.10.1        UGScIg                en8       \n127                127.0.0.1          UCS                   lo0       \n";
    assert_eq!(default_route_devices(t), ["en0", "en8"]);
}
