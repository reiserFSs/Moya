use std::path::PathBuf;
use anyhow::{Context, Result};

/// Per-user Moya data directory (`%APPDATA%\Moya`, `~/Library/Application Support/Moya`).
pub fn data_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    let directory = PathBuf::from(std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty()).context("APPDATA is unavailable")?).join("Moya");
    #[cfg(not(windows))]
    let directory = PathBuf::from(std::env::var_os("HOME")
        .context("HOME is unavailable")?).join("Library/Application Support/Moya");
    Ok(directory)
}

pub fn bridge_discovery_path() -> Result<PathBuf> {
    Ok(data_directory()?.join("bridge.json"))
}

/// Written by the panic hook; release builds on Windows have no console to show a panic.
#[cfg_attr(test, allow(dead_code))]
pub fn crash_log_path() -> Result<PathBuf> {
    Ok(data_directory()?.join("crash.log"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn discovery_is_in_user_config_directory() {
        let path = super::bridge_discovery_path().unwrap();
        #[cfg(windows)]
        assert_eq!(path, std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap()).join("Moya/bridge.json"));
        #[cfg(not(windows))]
        assert_eq!(path, std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("Library/Application Support/Moya/bridge.json"));
    }
}
