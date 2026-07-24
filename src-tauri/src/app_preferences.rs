use std::{
    fs,
    io::{Error, ErrorKind},
    path::{Path, PathBuf},
};

use crate::model::AppPreferences;

const SCHEMA_VERSION: u32 = 1;

pub fn read(data_root: &Path) -> std::io::Result<AppPreferences> {
    let path = path(data_root);
    if !path.is_file() {
        return Ok(AppPreferences::default());
    }
    let bytes = fs::read(path)?;
    let preferences = serde_json::from_slice::<AppPreferences>(&bytes).map_err(Error::other)?;
    if preferences.schema_version != SCHEMA_VERSION {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("不支持的应用行为设置版本：{}", preferences.schema_version),
        ));
    }
    Ok(preferences)
}

pub fn write(data_root: &Path, mut preferences: AppPreferences) -> std::io::Result<AppPreferences> {
    preferences.schema_version = SCHEMA_VERSION;
    let path = path(data_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&preferences).map_err(Error::other)?;
    crate::write_bytes_atomic(&path, &bytes)?;
    Ok(preferences)
}

#[cfg(windows)]
pub fn set_launch_at_login(enabled: bool) -> std::io::Result<()> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "EnvNexus AI";
    const LEGACY_VALUE_NAME: &str = "EnvPilot";

    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = current_user.create_subkey(RUN_KEY)?;
    if enabled {
        run.set_value(VALUE_NAME, &launch_command(std::env::current_exe()?))?;
        delete_value_if_present(&run, LEGACY_VALUE_NAME)
    } else {
        delete_value_if_present(&run, VALUE_NAME)?;
        delete_value_if_present(&run, LEGACY_VALUE_NAME)
    }
}

#[cfg(windows)]
fn delete_value_if_present(key: &winreg::RegKey, value_name: &str) -> std::io::Result<()> {
    match key.delete_value(value_name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(windows))]
pub fn set_launch_at_login(enabled: bool) -> std::io::Result<()> {
    if enabled {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "开机自启动目前仅支持 Windows",
        ));
    }
    Ok(())
}

fn launch_command(executable: PathBuf) -> String {
    format!("\"{}\"", executable.display())
}

fn path(data_root: &Path) -> PathBuf {
    data_root.join("config").join("app-preferences.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AppLanguage, CloseBehavior};

    #[test]
    fn defaults_preserve_normal_windows_close_behavior() {
        let temporary = tempfile::tempdir().unwrap();
        let preferences = read(temporary.path()).unwrap();
        assert_eq!(preferences.close_behavior, CloseBehavior::Exit);
        assert!(!preferences.start_minimized);
        assert!(!preferences.launch_at_login);
        assert_eq!(preferences.language, AppLanguage::SimplifiedChinese);
    }

    #[test]
    fn application_preferences_round_trip_in_data_root() {
        let temporary = tempfile::tempdir().unwrap();
        let preferences = AppPreferences {
            schema_version: 99,
            close_behavior: CloseBehavior::MinimizeToTray,
            start_minimized: true,
            launch_at_login: true,
            language: AppLanguage::English,
        };
        let saved = write(temporary.path(), preferences).unwrap();
        assert_eq!(saved.schema_version, SCHEMA_VERSION);
        assert_eq!(read(temporary.path()).unwrap(), saved);
    }

    #[test]
    fn launch_command_quotes_paths_with_spaces() {
        assert_eq!(
            launch_command(PathBuf::from(r"E:\Development Tools\EnvNexus-AI.exe")),
            r#""E:\Development Tools\EnvNexus-AI.exe""#
        );
    }
}
