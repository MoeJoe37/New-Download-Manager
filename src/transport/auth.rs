/// Send a request; a 401 asking for Digest authentication is answered with the URL's login (NDM's
/// header parser 0x58ad70 handles Basic and Digest), NTLM / Negotiate through Windows (`sspi`).
/// ureq itself sends Basic from the URL.
pub(crate) fn call_legacy(r: ureq::Request) -> Result<ureq::Response, String> {
    match r.clone().call() {
        Err(ureq::Error::Status(401, resp)) => {
            let challenges: Vec<String> = resp
                .all("WWW-Authenticate")
                .iter()
                .map(|s| s.to_string())
                .collect();
            let challenge = challenges.first().cloned().unwrap_or_default();
            let u = url::Url::parse(r.url()).map_err(|e| e.to_string())?;
            if let Some(pkg) = ["Negotiate", "NTLM"]
                .into_iter()
                .find(|p| challenges.iter().any(|c| c.trim().eq_ignore_ascii_case(p)))
            {
                let _ = resp.into_string(); // read it all so the connection goes back to the pool for the next leg
                return sspi(&r, &u, pkg);
            }
            match strip_ci(challenge.trim(), "Digest ").filter(|_| !u.username().is_empty()) {
                Some(params) => {
                    let (user, pass) = (
                        pct_decode(u.username()),
                        pct_decode(u.password().unwrap_or("")),
                    );
                    let uri = &u[url::Position::BeforePath..url::Position::AfterQuery];
                    let auth = digest(params, &user, &pass, r.method(), uri);
                    r.set("Authorization", &auth).call().map_err(err)
                }
                None => Err(err(ureq::Error::Status(401, resp))),
            }
        }
        x => x.map_err(err),
    }
}

/// NTLM / Negotiate: Windows builds the tokens (SSPI), with the URL's login ("DOMAIN\user" allowed) or,
/// without one, the signed-in Windows user. NTLM's legs must share the TCP connection: ureq's pool hands
/// the same keep-alive connection back as long as each response is read to the end.
/// Elsewhere Negotiate first tries Kerberos with the signed-in user's ticket (kinit, or the desktop's
/// sign-in), through the system's GSSAPI library (`gss`); then, as for NTLM, NTLMv2 is done here (MS-NLMP)
/// with the URL's login (SPNEGO servers accept raw NTLMSSP).
#[cfg(not(windows))]
fn sspi(r: &ureq::Request, u: &url::Url, pkg: &str) -> Result<ureq::Response, String> {
    let (login, pass) = (
        pct_decode(u.username()),
        pct_decode(u.password().unwrap_or("")),
    );
    let mut kerberos = None;
    if pkg == "Negotiate" {
        match gss::negotiate(r, u) {
            Ok(resp) => return Ok(resp),
            Err(e) => kerberos = Some(e),
        }
    }
    if login.is_empty() {
        return Err(match kerberos {
            Some(e) => format!("Kerberos sign-in failed ({e}); for NTLM put a user name and password in the address"),
            None => format!("{pkg} sign-in needs a user name and password in the address"),
        });
    }
    let (domain, user) = login.split_once('\\').unwrap_or(("", &login));
    let send = |token: &[u8]| {
        r.clone()
            .set("Authorization", &format!("{pkg} {}", b64(token)))
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(401, resp) => {
                    let next = resp
                        .all("WWW-Authenticate")
                        .iter()
                        .find_map(|c| strip_ci(c.trim(), &format!("{pkg} ")).map(str::to_string));
                    let _ = resp.into_string(); // the next leg goes on this keep-alive connection
                    next.ok_or("401 Unauthorized".to_string())
                }
                e => Err(err(e)),
            })
    };
    match send(&ntlm::negotiate()) {
        Ok(resp) => Ok(resp),
        Err(Ok(challenge)) => {
            let auth = ntlm::authenticate(&unb64(&challenge), user, domain, &pass)
                .ok_or(format!("{pkg} authentication failed"))?;
            send(&auth).map_err(|e| e.map_or_else(|e| e, |_| "401 Unauthorized".into()))
        }
        Err(Err(e)) => Err(e),
    }
}

/// Kerberos for Negotiate (SPNEGO, RFC 4559) through the system's GSSAPI library (MIT krb5 or Heimdal; GSS
/// on macOS), opened at run time: NDM runs without it, and a system without Kerberos simply has no ticket.
#[cfg(not(windows))]
mod gss {
    use super::{b64, err, strip_ci, unb64};
    use std::ffi::c_void;
    use std::sync::OnceLock;

    #[repr(C)]
    struct Buffer {
        length: usize,
        value: *mut c_void,
    }
    #[repr(C)]
    struct Oid {
        length: u32,
        elements: *const c_void,
    }
    type ImportName =
        unsafe extern "C" fn(*mut u32, *const Buffer, *const Oid, *mut *mut c_void) -> u32;
    type InitSecContext = unsafe extern "C" fn(
        *mut u32,
        *mut c_void,
        *mut *mut c_void,
        *mut c_void,
        *const Oid,
        u32,
        u32,
        *mut c_void,
        *const Buffer,
        *mut *const Oid,
        *mut Buffer,
        *mut u32,
        *mut u32,
    ) -> u32;
    type ReleaseBuffer = unsafe extern "C" fn(*mut u32, *mut Buffer) -> u32;
    type DeleteSecContext = unsafe extern "C" fn(*mut u32, *mut *mut c_void, *mut Buffer) -> u32;
    type ReleaseName = unsafe extern "C" fn(*mut u32, *mut *mut c_void) -> u32;
    struct Lib {
        import_name: ImportName,
        init: InitSecContext,
        release_buffer: ReleaseBuffer,
        delete: DeleteSecContext,
        release_name: ReleaseName,
    }

    /// GSS_C_NT_HOSTBASED_SERVICE (1.2.840.113554.1.2.1.4) and SPNEGO (1.3.6.1.5.5.2)
    const HOSTBASED: [u8; 10] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x01, 0x04];
    const SPNEGO: [u8; 6] = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
    const MUTUAL: u32 = 2;
    const CONTINUE_NEEDED: u32 = 1;

    fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(|| unsafe {
            // MIT krb5 or Heimdal on Linux, the system's GSS framework on macOS
            let h = [
                "libgssapi_krb5.so.2",
                "libgssapi.so.3",
                "/System/Library/Frameworks/GSS.framework/GSS",
            ]
            .iter()
            .map(|n| libc::dlopen(format!("{n}\0").as_ptr().cast(), libc::RTLD_NOW))
            .find(|h| !h.is_null())?;
            let sym = |n: &str| {
                Some(libc::dlsym(h, format!("{n}\0").as_ptr().cast())).filter(|p| !p.is_null())
            };
            Some(Lib {
                import_name: std::mem::transmute::<*mut c_void, ImportName>(sym(
                    "gss_import_name",
                )?),
                init: std::mem::transmute::<*mut c_void, InitSecContext>(sym(
                    "gss_init_sec_context",
                )?),
                release_buffer: std::mem::transmute::<*mut c_void, ReleaseBuffer>(sym(
                    "gss_release_buffer",
                )?),
                delete: std::mem::transmute::<*mut c_void, DeleteSecContext>(sym(
                    "gss_delete_sec_context",
                )?),
                release_name: std::mem::transmute::<*mut c_void, ReleaseName>(sym(
                    "gss_release_name",
                )?),
            })
        })
        .as_ref()
    }

    /// A security context for HTTP@host; each `step` turns the server's token into the next one to send.
    struct Ctx {
        lib: &'static Lib,
        name: *mut c_void,
        ctx: *mut c_void,
    }

    impl Ctx {
        fn new(host: &str) -> Result<Ctx, String> {
            let lib = lib().ok_or("no Kerberos (GSSAPI) library")?;
            let service = format!("HTTP@{host}");
            let buf = Buffer {
                length: service.len(),
                value: service.as_ptr() as *mut c_void,
            };
            let oid = Oid {
                length: HOSTBASED.len() as u32,
                elements: HOSTBASED.as_ptr().cast(),
            };
            let (mut minor, mut name) = (0, std::ptr::null_mut());
            let major = unsafe { (lib.import_name)(&mut minor, &buf, &oid, &mut name) };
            if major >> 16 != 0 {
                return Err(format!("GSSAPI error {major:#x}"));
            }
            Ok(Ctx {
                lib,
                name,
                ctx: std::ptr::null_mut(),
            })
        }

        /// The next token, and whether the server should answer with one more.
        fn step(&mut self, input: &[u8]) -> Result<(Vec<u8>, bool), String> {
            let mech = Oid {
                length: SPNEGO.len() as u32,
                elements: SPNEGO.as_ptr().cast(),
            };
            let inp = Buffer {
                length: input.len(),
                value: input.as_ptr() as *mut c_void,
            };
            let mut out = Buffer {
                length: 0,
                value: std::ptr::null_mut(),
            };
            let mut minor = 0;
            let null = std::ptr::null_mut();
            let major = unsafe {
                (self.lib.init)(
                    &mut minor,
                    null,
                    &mut self.ctx,
                    self.name,
                    &mech,
                    MUTUAL,
                    0,
                    null,
                    if input.is_empty() {
                        std::ptr::null()
                    } else {
                        &inp
                    },
                    std::ptr::null_mut(),
                    &mut out,
                    null.cast(),
                    null.cast(),
                )
            };
            let token = if out.value.is_null() {
                vec![]
            } else {
                unsafe { std::slice::from_raw_parts(out.value as *const u8, out.length).to_vec() }
            };
            unsafe { (self.lib.release_buffer)(&mut minor, &mut out) };
            if major >> 16 != 0 {
                // no ticket, an unknown service, an expired ticket...
                return Err(format!("GSSAPI error {major:#x}, minor {minor}"));
            }
            Ok((token, major & CONTINUE_NEEDED != 0))
        }
    }

    impl Drop for Ctx {
        fn drop(&mut self) {
            let mut minor = 0;
            unsafe {
                if !self.ctx.is_null() {
                    (self.lib.delete)(&mut minor, &mut self.ctx, std::ptr::null_mut());
                }
                (self.lib.release_name)(&mut minor, &mut self.name);
            }
        }
    }

    /// The request again with "Authorization: Negotiate <token>", for as many legs as the server needs.
    pub(super) fn negotiate(r: &ureq::Request, u: &url::Url) -> Result<ureq::Response, String> {
        let host = u.host_str().ok_or("no host")?;
        let mut ctx = Ctx::new(host)?;
        let (mut token, _) = ctx.step(&[])?;
        for _ in 0..4 {
            match r
                .clone()
                .set("Authorization", &format!("Negotiate {}", b64(&token)))
                .call()
            {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(401, resp)) => {
                    let next = resp
                        .all("WWW-Authenticate")
                        .iter()
                        .find_map(|c| strip_ci(c.trim(), "Negotiate ").map(str::to_string));
                    let _ = resp.into_string();
                    let Some(next) = next else {
                        return Err("the server refused the Kerberos ticket".into());
                    };
                    let (t, _) = ctx.step(&unb64(&next))?;
                    token = t;
                }
                Err(e) => return Err(err(e)),
            }
        }
        Err("Kerberos sign-in didn't finish".into())
    }

    /// Against a real Kerberos site: NDM_KERBEROS_URL=http://host/protected cargo test kerberos_site -- --ignored
    /// (with a ticket from kinit)
    #[test]
    #[ignore]
    fn kerberos_site() {
        let url = std::env::var("NDM_KERBEROS_URL").expect("NDM_KERBEROS_URL");
        let resp = super::call_legacy(ureq::agent().get(&url)).unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn without_a_ticket_it_fails_cleanly() {
        // CI has no Kerberos realm: either no library, or a context that can't get a ticket
        if let Ok(mut c) = Ctx::new("ndm-test.invalid") {
            assert!(c.step(&[]).is_err());
        }
    }
}

/// NTLM's messages (MS-NLMP): NEGOTIATE, then AUTHENTICATE with an NTLMv2 response to the CHALLENGE.
#[cfg(not(windows))]
mod ntlm {
    use md5::Digest;

    // UNICODE | OEM | REQUEST_TARGET | NTLM | ALWAYS_SIGN | EXTENDED_SESSIONSECURITY | TARGET_INFO | 128 | 56
    const FLAGS: u32 = 0x0000_0001
        | 0x0000_0002
        | 0x0000_0004
        | 0x0000_0200
        | 0x0000_8000
        | 0x0008_0000
        | 0x0080_0000
        | 0x2000_0000
        | 0x8000_0000;

    pub fn negotiate() -> Vec<u8> {
        let mut m = b"NTLMSSP\0".to_vec();
        m.extend(1u32.to_le_bytes());
        m.extend(FLAGS.to_le_bytes());
        m.extend([0u8; 16]); // no domain, no workstation
        m
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
        let mut k = [0u8; 64];
        k[..key.len()].copy_from_slice(key); // keys here are 16 bytes
        let mut inner = md5::Md5::new();
        inner.update(k.map(|b| b ^ 0x36));
        parts.iter().for_each(|p| inner.update(p));
        let mut outer = md5::Md5::new();
        outer.update(k.map(|b| b ^ 0x5c));
        outer.update(inner.finalize());
        outer.finalize().into()
    }

    /// NTOWFv2: HMAC-MD5(MD4(password), UPPER(user) + domain), UTF-16LE.
    fn ntowf(user: &str, domain: &str, pass: &str) -> [u8; 16] {
        use md4::Md4;
        let nt: [u8; 16] = Md4::digest(utf16(pass)).into();
        hmac_md5(&nt, &[&utf16(&(user.to_uppercase() + domain))])
    }

    /// The AUTHENTICATE message for a CHALLENGE (server challenge at 24, target info at 40).
    pub fn authenticate(challenge: &[u8], user: &str, domain: &str, pass: &str) -> Option<Vec<u8>> {
        authenticate_at(challenge, user, domain, pass, client_nonce(), now())
    }

    fn authenticate_at(
        challenge: &[u8],
        user: &str,
        domain: &str,
        pass: &str,
        cc: [u8; 8],
        now: u64,
    ) -> Option<Vec<u8>> {
        if !challenge.starts_with(b"NTLMSSP\0\x02\0\0\0") {
            return None;
        }
        let server = challenge.get(24..32)?;
        let field = |at: usize| -> Option<&[u8]> {
            let len = u16::from_le_bytes(challenge.get(at..at + 2)?.try_into().ok()?) as usize;
            let off = u32::from_le_bytes(challenge.get(at + 4..at + 8)?.try_into().ok()?) as usize;
            challenge.get(off..off + len)
        };
        let info = field(40).unwrap_or(&[]);
        // the server's time when it sends one (MsvAvTimestamp, id 7); then no LMv2 response either
        let mut ts = None;
        let mut i = 0;
        while let (Some(id), Some(len)) = (info.get(i..i + 2), info.get(i + 2..i + 4)) {
            let (id, len) = (
                u16::from_le_bytes([id[0], id[1]]),
                u16::from_le_bytes([len[0], len[1]]) as usize,
            );
            if id == 7 && len == 8 {
                ts = info.get(i + 4..i + 12).map(|t| t.to_vec());
            }
            if id == 0 {
                break;
            }
            i += 4 + len;
        }
        let key = ntowf(user, domain, pass);
        let mut blob = vec![1, 1, 0, 0, 0, 0, 0, 0];
        blob.extend(ts.clone().unwrap_or_else(|| now.to_le_bytes().to_vec()));
        blob.extend(cc);
        blob.extend([0u8; 4]);
        blob.extend(info);
        blob.extend([0u8; 4]);
        let proof = hmac_md5(&key, &[server, &blob]);
        let nt = [&proof[..], &blob].concat();
        let lm = if ts.is_some() {
            vec![0u8; 24]
        } else {
            [&hmac_md5(&key, &[server, &cc])[..], &cc].concat()
        };
        let (d, u, ws) = (utf16(domain), utf16(user), Vec::new());
        // header: signature, type, 6 fields (len, max, offset), flags; then the payload
        let mut m = b"NTLMSSP\0".to_vec();
        m.extend(3u32.to_le_bytes());
        let mut payload: Vec<u8> = Vec::new();
        for part in [&lm, &nt, &d, &u, &ws, &Vec::new()] {
            let off = 64 + payload.len();
            m.extend((part.len() as u16).to_le_bytes());
            m.extend((part.len() as u16).to_le_bytes());
            m.extend((off as u32).to_le_bytes());
            payload.extend(part.iter());
        }
        m.extend(FLAGS.to_le_bytes());
        m.extend(payload);
        Some(m)
    }

    /// 100 ns ticks since 1601 (Windows FILETIME).
    fn now() -> u64 {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        (unix.as_nanos() / 100) as u64 + 116_444_736_000_000_000
    }

    fn client_nonce() -> [u8; 8] {
        use std::hash::BuildHasher;
        std::collections::hash_map::RandomState::new()
            .hash_one(now())
            .to_le_bytes()
    }

    #[test]
    fn ntlmv2_reference() {
        // MS-NLMP 4.2.4: User / Domain / Password, server challenge 0123456789abcdef, client challenge aa*8,
        // time 0, target info: NetBIOS domain "Domain", NetBIOS server "Server"
        assert_eq!(
            ntowf("User", "Domain", "Password"),
            [
                0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0,
                0x2e, 0x3f
            ]
        );
        let info: Vec<u8> = [
            &[2u8, 0, 12, 0][..],
            &utf16("Domain"),
            &[1, 0, 12, 0],
            &utf16("Server"),
            &[0, 0, 0, 0],
        ]
        .concat();
        let mut ch = b"NTLMSSP\0\x02\0\0\0".to_vec();
        ch.extend([0u8; 8]); // target name
        ch.extend(FLAGS.to_le_bytes());
        ch.extend([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        ch.extend([0u8; 8]);
        ch.extend((info.len() as u16).to_le_bytes());
        ch.extend((info.len() as u16).to_le_bytes());
        ch.extend(48u32.to_le_bytes());
        ch.extend(&info);
        let m = authenticate_at(&ch, "User", "Domain", "Password", [0xaa; 8], 0).unwrap();
        let nt_off = u32::from_le_bytes(m[24..28].try_into().unwrap()) as usize;
        // NTProofStr from the spec
        assert_eq!(
            &m[nt_off..nt_off + 16],
            &[
                0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef,
                0x6a, 0x1c
            ]
        );
    }
}

#[cfg(windows)]
fn sspi(r: &ureq::Request, u: &url::Url, pkg: &str) -> Result<ureq::Response, String> {
    use windows_sys::Win32::Security::Authentication::Identity::*;
    use windows_sys::Win32::Security::Credentials::SecHandle;
    #[repr(C)]
    struct Identity {
        user: *const u16,
        user_len: u32,
        domain: *const u16,
        domain_len: u32,
        pass: *const u16,
        pass_len: u32,
        flags: u32,
    }
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let (login, pass) = (
        pct_decode(u.username()),
        pct_decode(u.password().unwrap_or("")),
    );
    let (domain, user) = login.split_once('\\').unwrap_or(("", &login));
    let (wu, wd, wp) = (w(user), w(domain), w(&pass));
    let id = Identity {
        user: wu.as_ptr(),
        user_len: wu.len() as u32,
        domain: wd.as_ptr(),
        domain_len: wd.len() as u32,
        pass: wp.as_ptr(),
        pass_len: wp.len() as u32,
        flags: 2,
    }; // SEC_WINNT_AUTH_IDENTITY_UNICODE
    let spn: Vec<u16> = format!("HTTP/{}", u.host_str().unwrap_or(""))
        .encode_utf16()
        .chain([0])
        .collect();
    let pkg_w: Vec<u16> = pkg.encode_utf16().chain([0]).collect();
    unsafe {
        let mut cred: SecHandle = std::mem::zeroed();
        let auth = if login.is_empty() {
            std::ptr::null()
        } else {
            &id as *const Identity as *const core::ffi::c_void
        };
        if AcquireCredentialsHandleW(
            std::ptr::null(),
            pkg_w.as_ptr(),
            SECPKG_CRED_OUTBOUND,
            std::ptr::null(),
            auth,
            None,
            std::ptr::null(),
            &mut cred,
            std::ptr::null_mut(),
        ) != 0
        {
            return Err(format!("{pkg} authentication is not available"));
        }
        let mut ctx: SecHandle = std::mem::zeroed();
        let mut input: Vec<u8> = Vec::new();
        let mut result = Err(format!("{pkg} authentication failed"));
        for leg in 0..3 {
            let mut out_buf = SecBuffer {
                cbBuffer: 0,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: std::ptr::null_mut(),
            };
            let mut out = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 1,
                pBuffers: &mut out_buf,
            };
            let mut in_buf = SecBuffer {
                cbBuffer: input.len() as u32,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: input.as_mut_ptr().cast(),
            };
            let in_desc = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 1,
                pBuffers: &mut in_buf,
            };
            let (mut attrs, mut expiry) = (0u32, 0i64);
            let st = InitializeSecurityContextW(
                &cred,
                if leg == 0 { std::ptr::null() } else { &ctx },
                spn.as_ptr(),
                ISC_REQ_ALLOCATE_MEMORY | ISC_REQ_CONNECTION,
                0,
                SECURITY_NATIVE_DREP,
                if leg == 0 { std::ptr::null() } else { &in_desc },
                0,
                &mut ctx,
                &mut out,
                &mut attrs,
                &mut expiry,
            );
            if st < 0 {
                break;
            }
            let token = std::slice::from_raw_parts(
                out_buf.pvBuffer as *const u8,
                out_buf.cbBuffer as usize,
            )
            .to_vec();
            FreeContextBuffer(out_buf.pvBuffer);
            result = r
                .clone()
                .set("Authorization", &format!("{pkg} {}", b64(&token)))
                .call()
                .map_err(|e| match e {
                    ureq::Error::Status(401, resp) => {
                        // the server's next challenge: "NTLM <base64>"
                        let next = resp.all("WWW-Authenticate").iter().find_map(|c| {
                            strip_ci(c.trim(), &format!("{pkg} ")).map(str::to_string)
                        });
                        let _ = resp.into_string();
                        next.map_or("401 Unauthorized".to_string(), |n| format!("\u{1}{n}"))
                    }
                    e => err(e),
                });
            match &result {
                Err(e) if e.starts_with('\u{1}') => input = unb64(&e[1..]),
                _ => break,
            }
        }
        DeleteSecurityContext(&ctx);
        FreeCredentialsHandle(&cred);
        result.map_err(|e| {
            if e.starts_with('\u{1}') {
                "401 Unauthorized".into()
            } else {
                e
            }
        })
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn b64(data: &[u8]) -> String {
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            s.push(if k <= c.len() {
                B64[(n >> (18 - 6 * k) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

fn unb64(s: &str) -> Vec<u8> {
    let bits: Vec<u32> = s
        .bytes()
        .filter_map(|b| B64.iter().position(|x| *x == b).map(|v| v as u32))
        .collect();
    let mut out = Vec::new();
    for c in bits.chunks(4) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |n, (k, v)| n | v << (18 - 6 * k));
        out.extend_from_slice(&n.to_be_bytes()[1..c.len()]);
    }
    out
}

/// RFC 7616 Digest response (MD5, qop "auth" when offered) for the challenge's parameters.
pub(crate) fn digest(params: &str, user: &str, pass: &str, method: &str, uri: &str) -> String {
    let mut p = std::collections::HashMap::new();
    let mut rest = params.trim();
    while let Some((k, v)) = rest.split_once('=') {
        let v = v.trim_start();
        let (val, tail) = match v.strip_prefix('"') {
            Some(q) => q.split_once('"').unwrap_or((q, "")),
            None => v.split_once(',').map_or((v, ""), |(a, b)| (a, b)),
        };
        p.insert(
            k.trim().trim_start_matches(',').trim().to_ascii_lowercase(),
            val.to_string(),
        );
        rest = tail.trim_start_matches([',', ' ']);
    }
    let g = |k: &str| p.get(k).cloned().unwrap_or_default();
    let (realm, nonce) = (g("realm"), g("nonce"));
    let ha1 = md5_hex(&format!("{user}:{realm}:{pass}"));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let cnonce = format!(
        "{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    );
    let qop = g("qop").split(',').map(str::trim).any(|q| q == "auth");
    let mut h = format!(
        r#"Digest username="{user}", realm="{realm}", nonce="{nonce}", uri="{uri}", algorithm=MD5"#
    );
    if qop {
        let resp = md5_hex(&format!("{ha1}:{nonce}:00000001:{cnonce}:auth:{ha2}"));
        h += &format!(r#", response="{resp}", qop=auth, nc=00000001, cnonce="{cnonce}""#);
    } else {
        h += &format!(
            r#", response="{}""#,
            md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
        );
    }
    if p.contains_key("opaque") {
        h += &format!(r#", opaque="{}""#, g("opaque"));
    }
    h
}

/// MD5 from Windows' CNG (BCryptHash with the built-in MD5 provider handle).
#[cfg(windows)]
pub(crate) fn md5_hex(s: &str) -> String {
    use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_MD5_ALG_HANDLE};
    let mut out = [0u8; 16];
    unsafe {
        BCryptHash(
            BCRYPT_MD5_ALG_HANDLE,
            std::ptr::null(),
            0,
            s.as_ptr(),
            s.len() as u32,
            out.as_mut_ptr(),
            16,
        )
    };
    out.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(not(windows))]
pub(crate) fn md5_hex(s: &str) -> String {
    use md5::Digest;
    md5::Md5::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => crate::transport::http_error(code),
        ureq::Error::Transport(error) => {
            use ureq::ErrorKind::*;
            match error.kind() {
                ProxyUnauthorized => "HTTP 407 Proxy Authentication Required".into(),
                InvalidProxyUrl => "Invalid proxy configuration".into(),
                ProxyConnect => "Could not connect through the configured proxy".into(),
                kind => format!("HTTP transport error: {kind:?}"),
            }
        }
    }
}

pub(crate) fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|p| p.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

pub(crate) fn pct_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{call, client::agent, request};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    /// NTLM's first leg on one keep-alive connection: 401 "NTLM" -> the request carries Windows' NTLMSSP
    /// negotiate message (the server here accepts it without the challenge leg).
    #[test]
    fn ntlm_negotiate_leg() {
        assert_eq!(
            [b64(b"ab"), b64(b"abc"), b64(b"a")],
            ["YWI=", "YWJj", "YQ=="]
        );
        assert_eq!(unb64(&b64(b"\x00\x01binary\xff")), b"\x00\x01binary\xff");
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://u:p@{}/f", l.local_addr().unwrap());
        thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            for _ in 0..2 {
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.strip_prefix("Authorization: NTLM ") {
                        auth = v.trim().to_string();
                    }
                }
                let reply = if unb64(&auth).starts_with(b"NTLMSSP\0\x01") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: NTLM\r\nContent-Length: 0\r\n\r\n"
                };
                (&s).write_all(reply.as_bytes()).unwrap();
            }
        });
        let resp = call(request(&agent("", ""), &url, "", "")).unwrap();
        assert_eq!(resp.into_string().unwrap(), "ok");
    }
}
