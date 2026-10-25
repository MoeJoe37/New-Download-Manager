// Gives ndm.exe its icon in Explorer: compile assets/ndm.ico with the Windows SDK's rc.exe and link the .res.
// No SDK found = no file icon (the window and tray still use assets/ndm.png).
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/ndm.ico");    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut sdks: Vec<PathBuf> = std::fs::read_dir(&kits).into_iter().flatten().flatten().map(|e| e.path().join(r"x64\rc.exe")).filter(|p| p.exists()).collect();
    sdks.sort();
    let Some(rc) = sdks.pop().or_else(|| Some(kits.join(r"x64\rc.exe")).filter(|p| p.exists())) else {
        return println!("cargo:warning=rc.exe not found, ndm.exe gets no file icon");
    };
    let ico = std::fs::canonicalize("assets/ndm.ico").unwrap();
    let script = out.join("ndm.rc");
    std::fs::write(&script, format!("1 ICON \"{}\"\n", ico.display().to_string().trim_start_matches(r"\\?\").replace('\\', "\\\\"))).unwrap();
    let res = out.join("ndm.res");
    let ok = std::process::Command::new(rc).arg("/nologo").arg("/fo").arg(&res).arg(&script).status().is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        println!("cargo:warning=rc.exe failed, ndm.exe gets no file icon");
    }
}
