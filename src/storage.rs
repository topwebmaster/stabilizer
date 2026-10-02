use crate::{
    model::{Event, Rule},
    systemd::Applied,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    pub rules: Vec<Rule>,
    pub applied: Vec<Applied>,
    pub events: Vec<Event>,
}

pub fn path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .context("Не определена домашняя папка")?;
    anyhow::ensure!(base.is_absolute(), "Папка состояния должна быть абсолютной");
    Ok(base.join("stabilizer/state.json"))
}

pub fn load(path: &Path) -> Result<Store> {
    match fs::read(path) {
        Ok(bytes) => {
            let state: Store = serde_json::from_slice(&bytes)
                .context("Файл правил повреждён; он не будет перезаписан")?;
            anyhow::ensure!(state.version == 1, "Неизвестная версия файла правил");
            for rule in &state.rules {
                rule.validate()?;
            }
            Ok(state)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store {
            version: 1,
            ..Default::default()
        }),
        Err(e) => Err(e.into()),
    }
}

pub fn save(path: &Path, state: &Store) -> Result<()> {
    let parent = path.parent().context("Неверный путь состояния")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".state-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(state)?)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

impl Store {
    pub fn event(&mut self, message: String) {
        self.events.push(Event {
            unix_time: crate::model::now(),
            message,
        });
        if self.events.len() > 100 {
            self.events.drain(..self.events.len() - 100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_state_is_preserved_and_atomic_save_roundtrips() {
        let dir =
            std::env::temp_dir().join(format!("stabilizer-state-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        fs::write(&path, b"not-json").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not-json");
        let mut store = Store {
            version: 1,
            ..Default::default()
        };
        store.event("Test".into());
        save(&path, &store).unwrap();
        assert_eq!(load(&path).unwrap().events[0].message, "Test");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
