//! Opt-in plain HTTP capture for one macOS network service.
//! SystemConfiguration owns the preferences transaction; the local journal survives an NDM crash.
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_void, CStr};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

static TRANSACTION: Mutex<()> = Mutex::new(());

type Ref = *const c_void;
type Mutable = *mut c_void;
const UTF8: u32 = 0x0800_0100;
const SINT64: i32 = 4;
const AUTH_INTERACTION_ALLOWED: u32 = 1 << 0;
const AUTH_EXTEND_RIGHTS: u32 = 1 << 1;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberIsFloatType(value: Ref) -> u8;
    fn CFNumberCreate(allocator: Ref, ty: i32, value: *const i64) -> Ref;
    fn CFNumberGetValue(value: Ref, ty: i32, out: *mut i64) -> u8;
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFDictionaryCreateMutable(
        allocator: Ref,
        capacity: isize,
        keys: Ref,
        values: Ref,
    ) -> Mutable;
    fn CFDictionaryCreateMutableCopy(allocator: Ref, capacity: isize, dictionary: Ref) -> Mutable;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFDictionaryGetCount(dictionary: Ref) -> isize;
    fn CFDictionarySetValue(dictionary: Mutable, key: Ref, value: Ref);
    fn CFDictionaryRemoveValue(dictionary: Mutable, key: Ref);
}

#[link(name = "SystemConfiguration", kind = "framework")]
extern "C" {
    fn SCPreferencesCreate(allocator: Ref, name: Ref, prefs: Ref) -> Ref;
    fn SCPreferencesCreateWithAuthorization(
        allocator: Ref,
        name: Ref,
        prefs: Ref,
        authorization: Mutable,
    ) -> Ref;
    fn SCPreferencesLock(preferences: Ref, wait: u8) -> u8;
    fn SCPreferencesUnlock(preferences: Ref) -> u8;
    fn SCPreferencesCommitChanges(preferences: Ref) -> u8;
    fn SCPreferencesApplyChanges(preferences: Ref) -> u8;
    fn SCNetworkSetCopyCurrent(preferences: Ref) -> Ref;
    fn SCNetworkSetCopyServices(set: Ref) -> Ref;
    fn SCNetworkServiceCopy(preferences: Ref, service_id: Ref) -> Ref;
    fn SCNetworkServiceGetEnabled(service: Ref) -> u8;
    fn SCNetworkServiceGetServiceID(service: Ref) -> Ref;
    fn SCNetworkServiceGetName(service: Ref) -> Ref;
    fn SCNetworkServiceCopyProtocol(service: Ref, protocol_type: Ref) -> Ref;
    fn SCNetworkProtocolGetEnabled(protocol: Ref) -> u8;
    fn SCNetworkProtocolGetConfiguration(protocol: Ref) -> Ref;
    fn SCNetworkProtocolSetConfiguration(protocol: Ref, configuration: Ref) -> u8;
    fn SCError() -> i32;
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn AuthorizationCreate(
        rights: Ref,
        environment: Ref,
        flags: u32,
        authorization: *mut Mutable,
    ) -> i32;
    fn AuthorizationFree(authorization: Mutable, flags: u32) -> i32;
}

/// A reference returned under Core Foundation's Create/Copy rule.
struct Cf(Ref);
impl Cf {
    fn owned(value: Ref) -> Option<Self> {
        if value.is_null() {
            None
        } else {
            Some(Self(value))
        }
    }

    fn string(text: &str) -> Self {
        Self::owned(unsafe {
            CFStringCreateWithBytes(null(), text.as_ptr(), text.len() as isize, UTF8, 0)
        })
        .expect("UTF-8 Core Foundation string")
    }

    fn number(value: i64) -> Self {
        Self::owned(unsafe { CFNumberCreate(null(), SINT64, &value) })
            .expect("Core Foundation number")
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn string(value: Ref) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let mut buffer = [0; 1024];
    if unsafe {
        CFGetTypeID(value) != CFStringGetTypeID()
            || CFStringGetCString(value, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) == 0
    } {
        return Err("macOS returned an unsupported network preference.".into());
    }
    Ok(Some(
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    ))
}

fn number(value: Ref) -> Result<Option<i64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let mut n = 0;
    if unsafe {
        CFGetTypeID(value) != CFNumberGetTypeID()
            || CFNumberIsFloatType(value) != 0
            || CFNumberGetValue(value, SINT64, &mut n) == 0
    } {
        return Err("macOS returned an unsupported numeric network preference.".into());
    }
    Ok(Some(n))
}

struct Authorization(Mutable);
impl Drop for Authorization {
    fn drop(&mut self) {
        unsafe { AuthorizationFree(self.0, 0) };
    }
}

struct Preferences {
    cf: Cf,
    _authorization: Option<Authorization>,
}

impl Preferences {
    fn new(write: bool) -> Result<Self, String> {
        let name = Cf::string("New Download Manager");
        let (value, authorization) = if write {
            let mut auth = null_mut();
            let status = unsafe {
                AuthorizationCreate(
                    null(),
                    null(),
                    AUTH_INTERACTION_ALLOWED | AUTH_EXTEND_RIGHTS,
                    &mut auth,
                )
            };
            if status != 0 || auth.is_null() {
                return Err(format!("macOS authorization failed ({status})."));
            }
            let auth = Authorization(auth);
            (
                unsafe { SCPreferencesCreateWithAuthorization(null(), name.0, null(), auth.0) },
                Some(auth),
            )
        } else {
            (unsafe { SCPreferencesCreate(null(), name.0, null()) }, None)
        };
        Ok(Self {
            cf: Cf::owned(value).ok_or_else(|| sc_error("Open network preferences"))?,
            _authorization: authorization,
        })
    }

    fn lock(&self) -> Result<Locked<'_>, String> {
        if unsafe { SCPreferencesLock(self.cf.0, 0) } == 0 {
            return Err(sc_error("Lock network preferences"));
        }
        Ok(Locked(self))
    }
}

struct Locked<'a>(&'a Preferences);
impl Drop for Locked<'_> {
    fn drop(&mut self) {
        unsafe { SCPreferencesUnlock(self.0.cf.0) };
    }
}

impl Locked<'_> {
    fn set(&self, protocol: &Cf, dictionary: Ref) -> Result<(), String> {
        self.save(protocol, dictionary)?;
        self.apply()
    }

    /// The change, saved to the system's preferences but not applied yet. An error here leaves the
    /// preferences as they were (nothing was committed).
    fn save(&self, protocol: &Cf, dictionary: Ref) -> Result<(), String> {
        if unsafe { SCNetworkProtocolSetConfiguration(protocol.0, dictionary) } == 0 {
            return Err(sc_error("Set web proxy"));
        }
        if unsafe { SCPreferencesCommitChanges(self.0.cf.0) } == 0 {
            return Err(sc_error("Save web proxy"));
        }
        Ok(())
    }

    fn apply(&self) -> Result<(), String> {
        if unsafe { SCPreferencesApplyChanges(self.0.cf.0) } == 0 {
            return Err(sc_error("Apply web proxy"));
        }
        Ok(())
    }
}

fn sc_error(action: &str) -> String {
    format!(
        "{action} failed (macOS error {}). Administrator authorization may be required.",
        unsafe { SCError() }
    )
}

fn current_services(prefs: &Preferences) -> Result<Cf, String> {
    let set = Cf::owned(unsafe { SCNetworkSetCopyCurrent(prefs.cf.0) })
        .ok_or_else(|| sc_error("Read current network location"))?;
    Cf::owned(unsafe { SCNetworkSetCopyServices(set.0) })
        .ok_or_else(|| sc_error("Read network services"))
}

pub fn services() -> Result<Vec<(String, String)>, String> {
    let prefs = Preferences::new(false)?;
    let services = current_services(&prefs)?;
    let mut choices = Vec::new();
    for index in 0..unsafe { CFArrayGetCount(services.0) } {
        let service = unsafe { CFArrayGetValueAtIndex(services.0, index) };
        if unsafe { SCNetworkServiceGetEnabled(service) } == 0 {
            continue;
        }
        if let (Some(id), Some(name)) = (
            string(unsafe { SCNetworkServiceGetServiceID(service) })?,
            string(unsafe { SCNetworkServiceGetName(service) })?,
        ) {
            choices.push((id, name));
        }
    }
    Ok(choices)
}

fn proxy_protocol(prefs: &Preferences, id: &str, enabling: bool) -> Result<Cf, String> {
    if enabling {
        let services = current_services(prefs)?;
        let mut found = false;
        for index in 0..unsafe { CFArrayGetCount(services.0) } {
            let service = unsafe { CFArrayGetValueAtIndex(services.0, index) };
            if string(unsafe { SCNetworkServiceGetServiceID(service) })?.as_deref() == Some(id)
                && unsafe { SCNetworkServiceGetEnabled(service) } != 0
            {
                found = true;
                break;
            }
        }
        if !found {
            return Err(
                "The selected service is no longer enabled in the current network location.".into(),
            );
        }
    }
    let id = Cf::string(id);
    let service = Cf::owned(unsafe { SCNetworkServiceCopy(prefs.cf.0, id.0) })
        .ok_or("The selected network service is unavailable.")?;
    let name = Cf::string("Proxies");
    let protocol = Cf::owned(unsafe { SCNetworkServiceCopyProtocol(service.0, name.0) })
        .ok_or("The selected network service has no proxy configuration.")?;
    if enabling && unsafe { SCNetworkProtocolGetEnabled(protocol.0) } == 0 {
        return Err("The selected network service has disabled proxy configuration.".into());
    }
    Ok(protocol)
}

fn get(dictionary: Ref, key: &str) -> Ref {
    if dictionary.is_null() {
        return null();
    }
    unsafe { CFDictionaryGetValue(dictionary, Cf::string(key).0) }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WebProxy {
    enabled: Option<i64>,
    host: Option<String>,
    port: Option<i64>,
}

impl WebProxy {
    fn ndm() -> Self {
        Self {
            enabled: Some(1),
            host: Some("127.0.0.1".into()),
            port: Some(crate::core::PROXY_PORT.into()),
        }
    }

    fn read(dictionary: Ref) -> Result<Self, String> {
        Ok(Self {
            enabled: number(get(dictionary, "HTTPEnable"))?,
            host: string(get(dictionary, "HTTPProxy"))?,
            port: number(get(dictionary, "HTTPPort"))?,
        })
    }

    fn edit(&self, original: Ref) -> Result<Cf, String> {
        let dictionary = unsafe {
            if original.is_null() {
                CFDictionaryCreateMutable(
                    null(),
                    0,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                )
            } else {
                CFDictionaryCreateMutableCopy(null(), 0, original)
            }
        };
        let owned = Cf::owned(dictionary).ok_or("Could not copy the existing proxy settings.")?;
        for (key, value) in [
            ("HTTPEnable", self.enabled.map(Cf::number)),
            ("HTTPProxy", self.host.as_deref().map(Cf::string)),
            ("HTTPPort", self.port.map(Cf::number)),
        ] {
            let key = Cf::string(key);
            unsafe {
                if let Some(value) = value {
                    CFDictionarySetValue(dictionary, key.0, value.0);
                } else {
                    CFDictionaryRemoveValue(dictionary, key.0);
                }
            }
        }
        Ok(owned)
    }
}

#[derive(Serialize, Deserialize)]
struct Journal {
    id: String,
    absent: bool,
    previous: WebProxy,
}

fn journal_path() -> PathBuf {
    crate::core::store_path().with_file_name("mac-http-capture.json")
}

pub fn has_journal() -> bool {
    journal_path().exists()
}

fn write_journal(path: &Path, journal: &Journal) -> Result<(), String> {
    let result = (|| -> std::io::Result<()> {
        let parent = path.parent().expect("journal directory");
        fs::create_dir_all(parent)?;
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let tmp = path.with_extension(format!("{}.{}.new", std::process::id(), time.as_nanos()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        let written = (|| -> std::io::Result<()> {
            file.write_all(&serde_json::to_vec(journal)?)?;
            file.sync_all()?;
            // Publish without replacing a recovery record belonging to an earlier session.
            fs::hard_link(&tmp, path)?;
            Ok(())
        })();
        let _ = fs::remove_file(tmp);
        written
    })();
    result.map_err(|e| format!("Could not save proxy recovery data: {e}"))
}

pub fn enable(id: &str, ready: &AtomicBool) -> Result<(), String> {
    let _transaction = TRANSACTION.lock().unwrap();
    if !ready.load(Ordering::SeqCst) {
        return Err("NDM's local HTTP proxy is unavailable.".into());
    }
    let prefs = Preferences::new(true)?;
    let lock = prefs.lock()?;
    let protocol = proxy_protocol(&prefs, id, true)?;
    let current = unsafe { SCNetworkProtocolGetConfiguration(protocol.0) };
    let previous = WebProxy::read(current)?;
    if previous.enabled.unwrap_or_default() != 0 {
        return Err("This service already has a web proxy. Its settings were left intact.".into());
    }
    for key in [
        "ProxyAutoConfigEnable",
        "ProxyAutoDiscoveryEnable",
        "SOCKSEnable",
    ] {
        if number(get(current, key))?.unwrap_or_default() != 0 {
            return Err("This service already uses automatic proxy configuration or SOCKS.".into());
        }
    }
    let update = WebProxy::ndm().edit(current)?;
    let path = journal_path();
    write_journal(
        &path,
        &Journal {
            id: id.into(),
            absent: current.is_null(),
            previous,
        },
    )?;
    // Nothing committed (the administrator prompt was cancelled, say): the preferences are as they were,
    // so the record goes too, or every start and quit would try to restore them. Once committed the
    // record stays, even if applying fails.
    if let Err(error) = lock.save(&protocol, update.0) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    lock.apply()
}

pub fn restore() -> Result<(), String> {
    let _transaction = TRANSACTION.lock().unwrap();
    let path = journal_path();
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let journal: Journal = serde_json::from_reader(file.take(8192))
        .map_err(|_| "The macOS proxy recovery record is unreadable.")?;
    let prefs = Preferences::new(true)?;
    let lock = prefs.lock()?;
    let protocol = proxy_protocol(&prefs, &journal.id, false)?;
    let current = unsafe { SCNetworkProtocolGetConfiguration(protocol.0) };
    let state = WebProxy::read(current)?;
    if state == WebProxy::ndm() {
        let restored = journal.previous.edit(current)?;
        let configuration = if journal.absent && unsafe { CFDictionaryGetCount(restored.0) } == 0 {
            null()
        } else {
            restored.0
        };
        lock.set(&protocol, configuration)?;
    } else if state == journal.previous {
        // A previous attempt may have committed the settings without applying them.
        lock.apply()?;
    }
    // A different endpoint means the user or macOS replaced our proxy. Preserve that change.
    fs::remove_file(path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_edit_restores_absent_and_disabled_values_and_preserves_https() {
        let absent = WebProxy::read(null()).unwrap();
        let settings = WebProxy {
            enabled: Some(0),
            host: Some("old-proxy.example".into()),
            port: Some(3128),
        };
        let original = settings.edit(null()).unwrap();
        let key = Cf::string("HTTPSEnable");
        let value = Cf::number(1);
        unsafe { CFDictionarySetValue(original.0.cast_mut(), key.0, value.0) };
        let captured = WebProxy::ndm().edit(original.0).unwrap();
        assert_eq!(WebProxy::read(captured.0).unwrap(), WebProxy::ndm());
        let restored = settings.edit(captured.0).unwrap();
        assert_eq!(WebProxy::read(restored.0).unwrap(), settings);
        assert_eq!(number(get(restored.0, "HTTPSEnable")).unwrap(), Some(1));
        let cleared = absent.edit(restored.0).unwrap();
        assert_eq!(WebProxy::read(cleared.0).unwrap(), absent);
        assert_eq!(unsafe { CFDictionaryGetCount(cleared.0) }, 1);
    }

    #[test]
    fn recovery_record_is_required_before_proxy_changes() {
        let path = std::env::temp_dir().join(format!(
            "ndm-web-proxy-journal-{}/snapshot.json",
            std::process::id()
        ));
        let journal = Journal {
            id: "service-1".into(),
            absent: true,
            previous: WebProxy::read(null()).unwrap(),
        };
        write_journal(&path, &journal).unwrap();
        let second = Journal {
            id: "service-2".into(),
            ..journal
        };
        assert!(write_journal(&path, &second).is_err());
        let stored: Journal = serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert_eq!(stored.id, "service-1");
        assert!(enable("service-1", &AtomicBool::new(false)).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
