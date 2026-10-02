use crate::model::{AppGroup, Rule};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::MetadataExt, path::Path};
use zbus::{
    Connection, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue},
};

const DEST: &str = "org.freedesktop.systemd1";
const MANAGER: &str = "/org/freedesktop/systemd1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Applied {
    pub unit: String,
    pub key: String,
    pub invocation: Vec<u8>,
    pub previous_preference: String,
    pub previous_high: u64,
    pub last_preference: String,
    pub last_high: u64,
    #[serde(default)]
    pub pending_preference: Option<String>,
    #[serde(default)]
    pub pending_high: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Live {
    pub invocation: Vec<u8>,
    pub preference: String,
    pub high: u64,
    pub cgroup: String,
}

#[derive(Debug, Clone, Default)]
pub struct Coverage {
    pub effective: bool,
    pub message: String,
}

async fn manager(conn: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(conn, DEST, MANAGER, "org.freedesktop.systemd1.Manager").await?)
}

async fn unit_path(conn: &Connection, unit: &str) -> Result<OwnedObjectPath> {
    Ok(manager(conn).await?.call("GetUnit", &(unit,)).await?)
}

pub async fn live(conn: &Connection, unit: &str) -> Result<Live> {
    anyhow::ensure!(
        unit.ends_with(".service") || unit.ends_with(".scope"),
        "Неподдерживаемая группа"
    );
    let path = unit_path(conn, unit).await?;
    let common = Proxy::new(conn, DEST, path.as_str(), "org.freedesktop.systemd1.Unit").await?;
    let invocation: Vec<u8> = common.get_property("InvocationID").await?;
    let iface = if unit.ends_with(".scope") {
        "org.freedesktop.systemd1.Scope"
    } else {
        "org.freedesktop.systemd1.Service"
    };
    let unit_proxy = Proxy::new(conn, DEST, path.as_str(), iface).await?;
    Ok(Live {
        invocation,
        preference: unit_proxy.get_property("ManagedOOMPreference").await?,
        high: unit_proxy.get_property("MemoryHigh").await?,
        cgroup: unit_proxy.get_property("ControlGroup").await?,
    })
}

pub async fn coverage(conn: &Connection, uid: u32) -> Coverage {
    let result: Result<Coverage> = async {
        let path = unit_path(conn, "systemd-oomd.service").await?;
        let proxy = Proxy::new(conn, DEST, path.as_str(), "org.freedesktop.systemd1.Unit").await?;
        let active: String = proxy.get_property("ActiveState").await?;
        if active != "active" {
            return Ok(Coverage {
                effective: false,
                message: "oomd не запущен; правила можно настроить, но защита пока не подтверждена"
                    .to_string(),
            });
        }
        let monitored = [
            "-.slice".to_string(),
            "user.slice".to_string(),
            format!("user-{uid}.slice"),
            format!("user@{uid}.service"),
        ];
        let mut pressure_target = false;
        for unit in &monitored {
            let path = unit_path(conn, unit).await?;
            let iface = if unit.ends_with(".slice") {
                "org.freedesktop.systemd1.Slice"
            } else {
                "org.freedesktop.systemd1.Service"
            };
            let proxy = Proxy::new(conn, DEST, path.as_str(), iface).await?;
            let memory: String = proxy.get_property("ManagedOOMMemoryPressure").await?;
            let swap: String = proxy.get_property("ManagedOOMSwap").await?;
            let cgroup: String = proxy.get_property("ControlGroup").await?;
            let owner =
                fs::metadata(Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/')))?
                    .uid();
            // A user-owned preference is ignored when a root-owned ancestor selects a victim.
            if swap == "kill" || (memory == "kill" && owner != uid) {
                return Ok(Coverage {
                    effective: false,
                    message: format!(
                        "{unit}: системная политика может игнорировать пользовательскую защиту"
                    ),
                });
            }
            pressure_target |= memory == "kill" && owner == uid;
        }
        Ok(Coverage {
            effective: pressure_target,
            message: if pressure_target {
                "oomd активен · пользовательская защита поддерживается".to_string()
            } else {
                "Пользовательская сессия не отслеживается oomd".to_string()
            },
        })
    }
    .await;
    result.unwrap_or_else(|e| Coverage {
        effective: false,
        message: format!("Не удалось проверить политику oomd: {e}"),
    })
}

pub fn target_is_safe(app: &AppGroup, live: &Live, uid: u32) -> Result<()> {
    let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/");
    anyhow::ensure!(
        live.cgroup == app.cgroup && live.cgroup.starts_with(&prefix),
        "Группа приложения изменилась; повторите после обновления списка"
    );
    let path = Path::new("/sys/fs/cgroup").join(live.cgroup.trim_start_matches('/'));
    anyhow::ensure!(
        fs::metadata(&path)?.uid() == uid,
        "Группа принадлежит другому пользователю"
    );
    // Preferences are not recursive. Do not promise protection for a container of child cgroups.
    anyhow::ensure!(
        !fs::read_dir(&path)?
            .filter_map(Result::ok)
            .any(|e| e.path().is_dir()),
        "Вложенные группы процессов: первая версия не может подтвердить защиту всей группы"
    );
    // An ancestor marked as one indivisible OOM group can take the protected child down with it.
    let mut ancestor = path.parent();
    while let Some(dir) = ancestor {
        if dir == Path::new("/sys/fs/cgroup") {
            break;
        }
        let grouped = fs::read_to_string(dir.join("memory.oom.group")).unwrap_or_default();
        anyhow::ensure!(
            grouped.trim() != "1",
            "Родительская группа может быть завершена целиком: {}",
            dir.display()
        );
        ancestor = dir.parent();
    }
    Ok(())
}

pub fn verify_kernel_settings(cgroup: &str, preference: &str, high: u64) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'));
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let marked = |name: &std::ffi::CStr| -> Result<bool> {
        let mut value = [0_u8; 8];
        // Pointers refer to live, NUL-terminated strings and a writable buffer of the declared size.
        let length = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if length < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENODATA) {
                return Ok(false);
            }
            return Err(error.into());
        }
        Ok(length == 1 && value[0] == b'1')
    };
    let omit = marked(c"user.oomd_omit")?;
    let avoid = marked(c"user.oomd_avoid")?;
    let actual = if omit {
        "omit"
    } else if avoid {
        "avoid"
    } else {
        "none"
    };
    anyhow::ensure!(
        actual == preference,
        "Атрибут защиты cgroup не соответствует настройке systemd"
    );
    let actual_high = fs::read_to_string(path.join("memory.high"))?;
    let actual_high = if actual_high.trim() == "max" {
        u64::MAX
    } else {
        actual_high.trim().parse()?
    };
    anyhow::ensure!(
        actual_high == high,
        "Лимит cgroup не соответствует настройке systemd"
    );
    Ok(())
}

pub async fn self_protection(
    conn: &Connection,
    uid: u32,
    coverage: bool,
) -> crate::model::SelfProtection {
    let result: Result<crate::model::SelfProtection> = async {
        let cgroups = fs::read_to_string("/proc/self/cgroup")?;
        let cgroup = cgroups
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .context("Не определена группа агента")?;
        let (_, unit) = crate::monitor::managed_group(cgroup, uid)
            .context("Агент вне пользовательской службы")?;
        anyhow::ensure!(
            unit == "stabilizer-agent.service" || unit == "stabilizer-agent-dev.service",
            "Агент запущен без собственной службы"
        );
        let live = live(conn, &unit).await?;
        verify_kernel_settings(&live.cgroup, &live.preference, live.high)?;
        let path = unit_path(conn, &unit).await?;
        let service = Proxy::new(
            conn,
            DEST,
            path.as_str(),
            "org.freedesktop.systemd1.Service",
        )
        .await?;
        let restart: String = service.get_property("Restart").await?;
        let oomd = live.preference == "omit" && coverage;
        let auto_restart = restart == "always";
        Ok(crate::model::SelfProtection {
            oomd,
            auto_restart,
            detail: if oomd && auto_restart {
                "Защита от oomd и автоперезапуск активны".into()
            } else {
                "Самозащита настроена частично".into()
            },
        })
    }
    .await;
    result.unwrap_or_else(|e| crate::model::SelfProtection {
        detail: e.to_string(),
        ..Default::default()
    })
}

pub fn prepare(app: &AppGroup, live: &Live, rule: &Rule) -> Applied {
    Applied {
        unit: app.unit.clone(),
        key: app.key.clone(),
        invocation: live.invocation.clone(),
        previous_preference: live.preference.clone(),
        previous_high: live.high,
        last_preference: rule.priority.preference().to_string(),
        last_high: rule
            .memory_high_mib
            .map(|m| m * 1024 * 1024)
            .unwrap_or(live.high),
        pending_preference: None,
        pending_high: None,
    }
}

pub async fn set(conn: &Connection, record: &Applied) -> Result<()> {
    let properties: Vec<(&str, OwnedValue)> = vec![
        (
            "ManagedOOMPreference",
            OwnedValue::from(zbus::zvariant::Str::from(record.last_preference.as_str())),
        ),
        ("MemoryHigh", OwnedValue::from(record.last_high)),
    ];
    manager(conn)
        .await?
        .call::<_, _, ()>(
            "SetUnitProperties",
            &(record.unit.as_str(), true, properties),
        )
        .await
        .context("systemd отклонил изменение правила")?;
    Ok(())
}

pub fn restoration(record: &Applied, observed: &Live) -> Result<Applied> {
    anyhow::ensure!(
        observed.invocation == record.invocation,
        "Экземпляр приложения уже сменился"
    );
    anyhow::ensure!(
        (observed.preference == record.last_preference && observed.high == record.last_high)
            || (record.pending_preference.as_deref() == Some(observed.preference.as_str())
                && record.pending_high == Some(observed.high)),
        "Настройки изменены другим инструментом; Stabilizer не будет их перезаписывать"
    );
    let mut reset = record.clone();
    reset.last_preference = record.previous_preference.clone();
    reset.last_high = record.previous_high;
    Ok(reset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn undo_does_not_overwrite_a_new_process_or_external_policy() {
        let record = Applied {
            unit: "x.service".into(),
            key: "service:x.service".into(),
            invocation: vec![1],
            previous_preference: "none".into(),
            previous_high: u64::MAX,
            last_preference: "omit".into(),
            last_high: u64::MAX,
            pending_preference: None,
            pending_high: None,
        };
        let mut observed = Live {
            invocation: vec![1],
            preference: "omit".into(),
            high: u64::MAX,
            cgroup: String::new(),
        };
        assert_eq!(
            restoration(&record, &observed).unwrap().last_preference,
            "none"
        );
        observed.preference = "avoid".into();
        assert!(restoration(&record, &observed).is_err());
        observed.preference = "omit".into();
        observed.invocation = vec![2];
        assert!(restoration(&record, &observed).is_err());
    }
}
