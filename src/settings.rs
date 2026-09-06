//! Small, recoverable user preferences stored using atomic file replacement.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub refresh_secs: f64,
    pub scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_secs: 1.0,
            scale: 1.0,
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        self.refresh_secs = if self.refresh_secs.is_finite() {
            self.refresh_secs.clamp(0.5, 5.0)
        } else {
            Self::default().refresh_secs
        };
        self.scale = if self.scale.is_finite() {
            self.scale.clamp(1.0, 2.0)
        } else {
            Self::default().scale
        };
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_path()?)
    }

    fn save_to(&self, path: &Path) -> Result<(), String> {
        let mut normalized = self.clone();
        normalized.normalize();
        let mut contents = serde_json::to_vec_pretty(&normalized)
            .map_err(|error| format!("Could not encode settings: {error}"))?;
        contents.push(b'\n');
        let directory = path
            .parent()
            .ok_or("Settings path has no parent directory")?;
        fs::create_dir_all(directory)
            .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
        let (temporary_path, mut file) = create_temporary_file(directory)
            .map_err(|error| format!("Could not create temporary settings: {error}"))?;
        let result = (|| -> std::io::Result<()> {
            file.write_all(&contents)?;
            file.sync_all()?;
            fs::rename(&temporary_path, path)?;
            // Persist the rename itself as well as the file contents on Linux.
            File::open(directory)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary_path);
            return Err(format!("Could not save {}: {error}", path.display()));
        }
        Ok(())
    }
}

pub fn load() -> (Settings, Option<String>) {
    match config_path() {
        Ok(path) => load_from(&path),
        Err(error) => (Settings::default(), Some(error)),
    }
}

fn load_from(path: &Path) -> (Settings, Option<String>) {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Settings::default(), None);
        }
        Err(error) => {
            return (
                Settings::default(),
                Some(format!(
                    "Could not read {}: {error}. Using default settings.",
                    path.display()
                )),
            );
        }
    };
    let mut settings: Settings = match serde_json::from_slice(&contents) {
        Ok(settings) => settings,
        Err(error) => {
            return (
                Settings::default(),
                Some(format!(
                    "Could not parse {}: {error}. Using default settings.",
                    path.display()
                )),
            );
        }
    };
    let original = (settings.refresh_secs, settings.scale);
    settings.normalize();
    let warning = if original != (settings.refresh_secs, settings.scale) {
        Some("Settings were outside the supported range and have been adjusted.".to_owned())
    } else {
        None
    };
    (settings, warning)
}

fn config_path() -> Result<PathBuf, String> {
    // XDG paths must be absolute. An empty or relative override is ignored as
    // specified by XDG, so it cannot redirect writes into the working tree.
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })
        .ok_or("Cannot locate the settings directory: XDG_CONFIG_HOME and HOME are unavailable.")?;
    Ok(base.join("loadpeek").join("settings.json"))
}

fn create_temporary_file(directory: &Path) -> std::io::Result<(PathBuf, File)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..16 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".settings.{}.{timestamp}.{sequence}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not choose a unique temporary file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let (path, file) = create_temporary_file(&env::temp_dir()).unwrap();
            drop(file);
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn settings_path(&self) -> PathBuf {
            self.0.join("loadpeek").join("settings.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn invalid_values_become_finite_supported_values() {
        let mut settings = Settings {
            refresh_secs: f64::NAN,
            scale: f32::INFINITY,
        };
        settings.normalize();
        assert_eq!(settings.refresh_secs, 1.0);
        assert_eq!(settings.scale, 1.0);
        settings.refresh_secs = 99.0;
        settings.scale = -5.0;
        settings.normalize();
        assert_eq!(settings.refresh_secs, 5.0);
        assert_eq!(settings.scale, 1.0);
        settings.refresh_secs = -99.0;
        settings.scale = 99.0;
        settings.normalize();
        assert_eq!(settings.refresh_secs, 0.5);
        assert_eq!(settings.scale, 2.0);
    }

    #[test]
    fn absent_config_is_a_normal_first_launch() {
        let directory = TestDirectory::new();
        let (settings, warning) = load_from(&directory.settings_path());
        assert!(warning.is_none());
        assert_eq!(settings.refresh_secs, 1.0);
    }

    #[test]
    fn save_replaces_existing_config_and_cleans_up_temporary_files() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        Settings::default().save_to(&path).unwrap();
        Settings {
            refresh_secs: 0.5,
            scale: 1.5,
        }
        .save_to(&path)
        .unwrap();
        let (settings, warning) = load_from(&path);
        assert!(warning.is_none());
        assert_eq!(settings.refresh_secs, 0.5);
        assert_eq!(settings.scale, 1.5);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn partial_config_uses_defaults_and_reports_out_of_range_values() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"refresh_secs": 20}"#).unwrap();
        let (settings, warning) = load_from(&path);
        assert_eq!(settings.refresh_secs, 5.0);
        assert_eq!(settings.scale, 1.0);
        assert!(warning.is_some());
    }

    #[test]
    fn malformed_config_reports_a_recoverable_error() {
        let directory = TestDirectory::new();
        let path = directory.settings_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"refresh_secs": "fast"}"#).unwrap();
        let (settings, warning) = load_from(&path);
        assert_eq!(settings.refresh_secs, 1.0);
        assert!(warning.unwrap().contains("Using default settings"));
    }
}
