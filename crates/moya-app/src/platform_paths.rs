use std::path::PathBuf;
use anyhow::{Context, Result};

pub fn bridge_discovery_path() -> Result<PathBuf> {
    #[cfg(windows)]
    let directory = PathBuf::from(std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty()).context("APPDATA is unavailable")?).join("Moya");
    #[cfg(not(windows))]
    let directory = PathBuf::from(std::env::var_os("HOME")
        .context("HOME is unavailable")?).join("Library/Application Support/Moya");
    Ok(directory.join("bridge.json"))
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
