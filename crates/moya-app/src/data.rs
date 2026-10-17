use std::{
    env, fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use moya_core::presets::{Preset, parse_presets, serialize_presets};

pub fn config_path() -> PathBuf {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(windows) {
        env::var_os("APPDATA")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty() && Path::new(p).is_absolute())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
    };
    base.join("Moya/userPresets.json")
}

pub fn load_presets() -> Result<Vec<Preset>> {
    let path = config_path();
    migrate_presets(&path)?;
    match fs::read_to_string(&path) {
        Ok(json) => parse_presets(&json).with_context(|| format!("Reading {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("Reading {}", path.display())),
    }
}

fn temporary_presets_file(parent: &Path) -> Result<(PathBuf, fs::File)> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    loop {
        let temp = parent.join(format!(
            ".userPresets.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Creating temporary preset file"),
        }
    }
}

fn migrate_presets(path: &Path) -> Result<()> {
    if path.try_exists()? { return Ok(()); }
    let parent = path.parent().context("Preset path has no parent directory")?;
    // Read presets from the legacy config folder without changing it.
    let legacy = parent.parent().context("Config path has no parent directory")?
        .join("Oniric/userPresets.json");
    let mut source = match fs::File::open(&legacy) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Reading {}", legacy.display())),
    };
    fs::create_dir_all(parent)?;
    let (temp, mut file) = temporary_presets_file(parent)?;
    let result = (|| -> Result<()> {
        std::io::copy(&mut source, &mut file)?;
        file.sync_all()?;
        drop(file);
        // Publish a complete copy without replacing a concurrently created Moya file.
        match fs::hard_link(&temp, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.into()),
        }
    })();
    let _ = fs::remove_file(&temp);
    result.with_context(|| format!("Migrating {} to {}", legacy.display(), path.display()))
}

pub fn save_presets(presets: &[Preset]) -> Result<()> {
    let path = config_path();
    let parent = path.parent().context("Preset path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;
    let (temp, mut file) = temporary_presets_file(parent)?;
    let result = (|| -> Result<()> {
        file.write_all(serialize_presets(presets).as_bytes())?;
        file.sync_all()?;
        drop(file);
        replace_file(&temp, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("Saving {}", path.display()))
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // Same-directory rename with REPLACE_EXISTING; never delete the old file first.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_migrate_once_without_changing_legacy_or_existing_moya() -> Result<()> {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let root = env::temp_dir().join(format!("moya-presets-{}-{nonce}", std::process::id()));
        let path = root.join("Moya/userPresets.json");
        // Exercise migration from the legacy config folder.
        let legacy = root.join("Oniric/userPresets.json");
        migrate_presets(&path)?;
        assert!(!path.exists());
        fs::create_dir_all(legacy.parent().unwrap())?;
        let original = serialize_presets(&[Preset { name: "Legacy".into(), params: Default::default() }]);
        fs::write(&legacy, &original)?;
        migrate_presets(&path)?;
        assert_eq!(fs::read_to_string(&path)?, original);
        assert_eq!(fs::read_to_string(&legacy)?, original);
        assert_eq!(parse_presets(&fs::read_to_string(&path)?)?[0].name, "Legacy");
        fs::write(&path, "existing Moya file")?;
        fs::write(&legacy, "changed legacy file")?;
        migrate_presets(&path)?;
        assert_eq!(fs::read_to_string(&path)?, "existing Moya file");
        assert_eq!(fs::read_to_string(&legacy)?, "changed legacy file");
        assert_eq!(fs::read_dir(path.parent().unwrap())?.count(), 1);
        fs::remove_dir_all(root)?;
        Ok(())
    }

}
