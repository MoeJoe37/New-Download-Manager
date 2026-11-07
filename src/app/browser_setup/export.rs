//! Browser-specific copies; the bundled cross-browser extension is never modified.
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn write_export(
    source: &Path,
    parent: &Path,
    firefox: bool,
    guide: &str,
) -> io::Result<PathBuf> {
    let source = source.canonicalize()?;
    let parent = parent.canonicalize()?;
    // A destination inside the source would recursively copy the export into itself.
    if parent.starts_with(&source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a folder outside the browser extension's source folder.",
        ));
    }
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source.join("manifest.json"))?)?;
    let object = manifest.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "The extension manifest is invalid.",
        )
    })?;
    let background = object
        .get_mut("background")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "The extension manifest has no background scripts.",
            )
        })?;
    if firefox {
        background.remove("service_worker");
        object.remove("key");
        if let Some(permissions) = object
            .get_mut("permissions")
            .and_then(serde_json::Value::as_array_mut)
        {
            permissions.retain(|permission| permission != "downloads.ui");
        }
    } else {
        background.remove("scripts");
        object.remove("browser_specific_settings");
    }
    let name = if firefox {
        "NDM extension - Firefox"
    } else {
        "NDM extension - Chromium"
    };
    // Never replace a folder that a browser might already be using.
    let mut number = 1;
    let dest = loop {
        let path = parent.join(if number == 1 {
            name.to_owned()
        } else {
            format!("{name} ({number})")
        });
        match std::fs::create_dir(&path) {
            Ok(()) => break path,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => number += 1,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        crate::file_ops::copy_dir(&source, &dest)?;
        std::fs::write(
            dest.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        std::fs::write(dest.join("INSTALL.txt"), guide)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(error);
    }
    Ok(dest)
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
