use super::*;

/// Start with Windows (HKCU Run key), like NDM's "Launch... on startup".
/// Windows' (Internet Options) proxy: (ProxyEnable, ProxyServer, ProxyOverride).
pub fn system_proxy() -> (bool, String, String) {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    let text = |name: &str| {
        let (val, mut buf, mut len) = (wide(name), vec![0u16; 2048], 4096u32);
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != 0 { String::new() } else { String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]) }
    };
    let (val, mut on, mut len) = (wide("ProxyEnable"), 0u32, 4u32);
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut on as *mut u32).cast(), &mut len) };
    (r == 0 && on != 0, text("ProxyServer"), text("ProxyOverride"))
}

fn from_wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())])
}

fn ras_error(code: u32) -> String {
    use windows_sys::Win32::NetworkManagement::Rras::RasGetErrorStringW;
    let mut buf = [0u16; 512];
    if unsafe { RasGetErrorStringW(code, buf.as_mut_ptr(), 512) } == 0 { from_wide(&buf) } else { format!("Error {code}") }
}

/// Windows' dial-up / VPN connections (the phonebook entries).
pub fn ras_entries() -> Vec<String> {
    use windows_sys::Win32::NetworkManagement::Rras::{RasEnumEntriesW, RASENTRYNAMEW};
    let mut v: Vec<RASENTRYNAMEW> = (0..64).map(|_| unsafe { zeroed() }).collect();
    v[0].dwSize = size_of::<RASENTRYNAMEW>() as u32;
    let (mut bytes, mut n) = ((v.len() * size_of::<RASENTRYNAMEW>()) as u32, 0u32);
    if unsafe { RasEnumEntriesW(std::ptr::null(), std::ptr::null(), v.as_mut_ptr(), &mut bytes, &mut n) } != 0 {
        return vec![];
    }
    v[..n as usize].iter().map(|e| from_wide(&e.szEntryName)).collect()
}

fn ras_conns() -> Vec<windows_sys::Win32::NetworkManagement::Rras::RASCONNW> {
    use windows_sys::Win32::NetworkManagement::Rras::{RasEnumConnectionsW, RASCONNW};
    let mut v: Vec<RASCONNW> = (0..16).map(|_| unsafe { zeroed() }).collect();
    v[0].dwSize = size_of::<RASCONNW>() as u32;
    let (mut bytes, mut n) = ((v.len() * size_of::<RASCONNW>()) as u32, 0u32);
    if unsafe { RasEnumConnectionsW(v.as_mut_ptr(), &mut bytes, &mut n) } != 0 {
        return vec![];
    }
    v.truncate(n as usize);
    v
}

/// Is a dial-up / VPN connection up?
pub fn ras_connected() -> bool {
    use windows_sys::Win32::NetworkManagement::Rras::{RasGetConnectStatusW, RASCONNSTATUSW, RASCS_Connected};
    ras_conns().iter().any(|c| {
        let mut st: RASCONNSTATUSW = unsafe { zeroed() };
        st.dwSize = size_of::<RASCONNSTATUSW>() as u32;
        unsafe { RasGetConnectStatusW(c.hrasconn, &mut st) == 0 && st.rasconnstate == RASCS_Connected }
    })
}

/// The entry's dial parameters with the user name and password Windows keeps for it.
fn ras_params(entry: &str) -> windows_sys::Win32::NetworkManagement::Rras::RASDIALPARAMSW {
    use windows_sys::Win32::NetworkManagement::Rras::{RasGetEntryDialParamsW, RASDIALPARAMSW};
    let mut p: RASDIALPARAMSW = unsafe { zeroed() };
    p.dwSize = size_of::<RASDIALPARAMSW>() as u32;
    let name = wide(entry);
    p.szEntryName[..name.len().min(256)].copy_from_slice(&name[..name.len().min(256)]);
    let mut has_pass = 0;
    unsafe { RasGetEntryDialParamsW(std::ptr::null(), &mut p, &mut has_pass) };
    p
}

fn put(dst: &mut [u16], s: &str) {
    let w = wide(s);
    let n = w.len().min(dst.len());
    dst[..n].copy_from_slice(&w[..n]);
    dst[n - 1] = 0;
}

/// User name (and, with `save`, the password) Windows keeps for `entry` (Dial Up page's Apply).
pub fn ras_save(entry: &str, user: &str, pass: &str, save: bool) -> Result<(), String> {
    use windows_sys::Win32::NetworkManagement::Rras::RasSetEntryDialParamsW;
    let mut p = ras_params(entry);
    put(&mut p.szUserName, user);
    put(&mut p.szPassword, if save { pass } else { "" });
    match unsafe { RasSetEntryDialParamsW(std::ptr::null(), &p, !save as i32) } {
        0 => Ok(()),
        e => Err(ras_error(e)),
    }
}

/// Dials `entry` (synchronously) with Windows' saved user name and password.
pub fn ras_dial(entry: &str) -> Result<(), String> {
    use windows_sys::Win32::NetworkManagement::Rras::{RasDialW, RasHangUpW};
    let p = ras_params(entry);
    let mut h = std::ptr::null_mut();
    match unsafe { RasDialW(std::ptr::null(), std::ptr::null(), &p, 0, std::ptr::null(), &mut h) } {
        0 => Ok(()),
        e => {
            if !h.is_null() {
                unsafe { RasHangUpW(h) };
            }
            Err(ras_error(e))
        }
    }
}

/// "Hang up modem when done": every dial-up / VPN connection.
pub fn ras_hangup_all() {
    use windows_sys::Win32::NetworkManagement::Rras::RasHangUpW;
    for c in ras_conns() {
        unsafe { RasHangUpW(c.hrasconn) };
    }
}

/// Sets Windows' proxy (Internet Settings, as `system_proxy` reads it) and tells running programs.
pub fn set_system_proxy(on: bool, server: &str, bypass: &str) {
    use windows_sys::Win32::Networking::WinInet::{InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED};
    use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    let enable = on as u32;
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), wide("ProxyEnable").as_ptr(), REG_DWORD, (&enable as *const u32).cast(), 4);
        for (name, v) in [("ProxyServer", server), ("ProxyOverride", bypass)] {
            let v = wide(v);
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), wide(name).as_ptr(), REG_SZ, v.as_ptr().cast(), v.len() as u32 * 2);
        }
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
}

/// The proxy ("host:port") a PAC script picks for `url`, evaluated by WinHTTP; None = DIRECT or failure.
pub fn pac_proxy(pac: &str, url: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::*;
    if pac.trim().is_empty() {
        return None;
    }
    let (pac, url) = (wide(pac.trim()), wide(url));
    unsafe {
        let h = WinHttpOpen(std::ptr::null(), WINHTTP_ACCESS_TYPE_NO_PROXY, std::ptr::null(), std::ptr::null(), 0);
        if h.is_null() {
            return None;
        }
        let mut opts = WINHTTP_AUTOPROXY_OPTIONS { dwFlags: WINHTTP_AUTOPROXY_CONFIG_URL, dwAutoDetectFlags: 0, lpszAutoConfigUrl: pac.as_ptr(), lpvReserved: std::ptr::null_mut(), dwReserved: 0, fAutoLogonIfChallenged: 1 };
        let mut info: WINHTTP_PROXY_INFO = std::mem::zeroed();
        let ok = WinHttpGetProxyForUrl(h, url.as_ptr(), &mut opts, &mut info) != 0;
        WinHttpCloseHandle(h);
        let take = |p: *mut u16| {
            if p.is_null() {
                return String::new();
            }
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let t = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            GlobalFree(p.cast());
            t
        };
        let (list, _) = (take(info.lpszProxy), take(info.lpszProxyBypass));
        if !ok { return None; }
        if info.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY { return Some(String::new()); }
        // "[scheme=]host:port; ..." -> the first host:port
        let first = list.split([';', ' ']).find(|x| !x.is_empty())?;
        Some(first.rsplit('=').next().unwrap_or(first).to_string())
    }
}

/// The networks a download can be spread over: up, not loopback, with a default gateway.
pub fn nics() -> Vec<crate::net::Nic> {
    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
    use windows_sys::Win32::NetworkManagement::IpHelper::*;
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6};
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    // u64s keep the buffer aligned for the structs written into it
    let mut buf = vec![0u64; 16 * 1024 / 8];
    let mut out = vec![];
    // SAFETY: the list lives in `buf`, which outlives every pointer read from it; addresses are cast by their family
    unsafe {
        loop {
            let mut len = (buf.len() * 8) as u32;
            match GetAdaptersAddresses(AF_UNSPEC as u32, flags, std::ptr::null(), buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH, &mut len) {
                ERROR_SUCCESS => break,
                ERROR_BUFFER_OVERFLOW => buf = vec![0u64; len as usize / 8 + 1],
                _ => return out,
            }
        }
        let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !a.is_null() {
            let ad = &*a;
            a = ad.Next;
            if ad.OperStatus != IfOperStatusUp || ad.IfType == 24 || ad.FirstGatewayAddress.is_null() {
                continue;
            }
            let mut ips = vec![];
            let mut u = ad.FirstUnicastAddress;
            while !u.is_null() {
                let sa = (*u).Address.lpSockaddr;
                u = (*u).Next;
                if sa.is_null() {
                    continue;
                }
                match (*sa).sa_family {
                    AF_INET => ips.push(std::net::Ipv4Addr::from(u32::from_be((*(sa as *const SOCKADDR_IN)).sin_addr.S_un.S_addr)).into()),
                    AF_INET6 => {
                        let v6 = std::net::Ipv6Addr::from((*(sa as *const SOCKADDR_IN6)).sin6_addr.u.Byte);
                        if v6.segments()[0] & 0xffc0 != 0xfe80 {
                            ips.push(v6.into());
                        }
                    }
                    _ => {}
                }
            }
            let p = ad.FriendlyName;
            let name = if p.is_null() { String::new() } else { String::from_utf16_lossy(std::slice::from_raw_parts(p, (0..).take_while(|&i| *p.add(i) != 0).count())) };
            if !name.is_empty() && !ips.is_empty() {
                out.push(crate::net::Nic { name, ips });
            }
        }
    }
    out
}
