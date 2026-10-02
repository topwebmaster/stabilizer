use crate::model::{AppGroup, Memory, Snapshot};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, fs, path::Path};

fn display_name(fallback: &str) -> String {
    static NAMES: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let mut names = BTreeMap::new();
        let mut directories = vec![
            std::path::PathBuf::from("/usr/share/applications"),
            std::path::PathBuf::from("/usr/local/share/applications"),
            std::path::PathBuf::from("/var/lib/snapd/desktop/applications"),
            std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = std::path::PathBuf::from(home);
            directories.push(home.join(".local/share/flatpak/exports/share/applications"));
            directories.push(home.join(".local/share/applications"));
        }
        for directory in directories {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(id) = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.strip_suffix(".desktop"))
                else {
                    continue;
                };
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let section = text
                    .split("[Desktop Entry]")
                    .nth(1)
                    .unwrap_or("")
                    .split("\n[")
                    .next()
                    .unwrap_or("");
                let field = |key| section.lines().find_map(|s| s.strip_prefix(key));
                if let Some(name) = field("Name[ru]=").or_else(|| field("Name=")) {
                    names.insert(id.to_string(), name.to_string());
                }
            }
        }
        names
    });
    let snap = fallback.strip_prefix("snap.").map(|s| s.replace('.', "_"));
    names
        .get(fallback)
        .or_else(|| snap.as_ref().and_then(|id| names.get(id)))
        .or_else(|| {
            names
                .iter()
                .find(|(id, _)| id.starts_with(&format!("{fallback}-")))
                .map(|(_, name)| name)
        })
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

fn value(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|l| {
            l.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0)
}

fn pressure(text: &str, class: &str) -> f64 {
    text.lines()
        .find(|l| l.starts_with(class))
        .and_then(|l| {
            l.split_whitespace()
                .find_map(|s| s.strip_prefix("avg10="))?
                .parse()
                .ok()
        })
        .unwrap_or(0.0)
}

pub fn memory_from_text(meminfo: &str, psi: &str) -> Memory {
    let swap_total = value(meminfo, "SwapTotal:") * 1024;
    Memory {
        total: value(meminfo, "MemTotal:") * 1024,
        available: value(meminfo, "MemAvailable:") * 1024,
        swap_total,
        swap_used: swap_total.saturating_sub(value(meminfo, "SwapFree:") * 1024),
        pressure_some_avg10: pressure(psi, "some "),
        pressure_full_avg10: pressure(psi, "full "),
    }
}

fn unescape(unit: &str) -> String {
    let bytes = unit.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1] == b'x'
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 2..i + 4])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn identity(unit: &str) -> (String, String) {
    if unit.ends_with(".service") {
        let label = match unit {
            "dbus.service" => "D-Bus сессии",
            "org.gnome.Shell@ubuntu.service" => "GNOME Shell и дочерние процессы",
            _ => unit.trim_end_matches(".service"),
        };
        return (format!("service:{unit}"), label.to_string());
    }
    let decoded = unescape(unit.trim_end_matches(".scope"));
    let mut base = decoded.as_str();
    if let Some((head, tail)) = base.rsplit_once('-')
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        base = head;
    }
    if base.len() > 37 {
        let start = base.len() - 36;
        if base.is_char_boundary(start) && base.as_bytes()[start - 1] == b'-' {
            let uuid = &base[start..];
            if uuid.len() == 36
                && uuid.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
            {
                base = &base[..start - 1];
            }
        }
    }
    let label = base
        .strip_prefix("app-gnome-")
        .or_else(|| base.strip_prefix("app-flatpak-"))
        .or_else(|| base.strip_prefix("app-"))
        .unwrap_or(base)
        .to_string();
    (format!("app:{base}"), label)
}

pub fn managed_group(cgroup: &str, uid: u32) -> Option<(String, String)> {
    let prefix = format!("/user.slice/user-{uid}.slice/user@{uid}.service/");
    let tail = cgroup.strip_prefix(&prefix)?;
    let mut parts = tail.split('/');
    let mut built = prefix.trim_end_matches('/').to_string();
    for part in parts.by_ref() {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        built.push('/');
        built.push_str(part);
        if part.ends_with(".scope") || part.ends_with(".service") {
            return Some((built, part.to_string()));
        }
    }
    None
}

pub fn collect(uid: u32) -> Result<Snapshot> {
    let meminfo = fs::read_to_string("/proc/meminfo")?;
    let psi = fs::read_to_string("/proc/pressure/memory").unwrap_or_default();
    let mut groups: BTreeMap<String, (String, usize, u32, String)> = BTreeMap::new();
    for entry in fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        let Ok(status) = fs::read_to_string(dir.join("status")) else {
            continue;
        };
        if value(&status, "Uid:") != uid as u64 {
            continue;
        }
        let Ok(cgroups) = fs::read_to_string(dir.join("cgroup")) else {
            continue;
        };
        let Some(cgroup) = cgroups.lines().find_map(|l| l.strip_prefix("0::")) else {
            continue;
        };
        let Some((group, unit)) = managed_group(cgroup, uid) else {
            continue;
        };
        let exe = fs::read_link(dir.join("exe"))
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let record = groups.entry(group).or_insert((unit, 0, pid, exe.clone()));
        record.1 += 1;
        if pid < record.2 {
            record.2 = pid;
            record.3 = exe;
        }
    }
    let mut apps = Vec::new();
    for (cgroup, (unit, processes, _, executable)) in groups {
        let dir = Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'));
        let Ok(memory) = fs::read_to_string(dir.join("memory.current")) else {
            continue;
        };
        let Ok(memory) = memory.trim().parse::<u64>() else {
            continue;
        };
        let swap = fs::read_to_string(dir.join("memory.swap.current"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let (key, mut label) = identity(&unit);
        if !unit.ends_with(".service") {
            label = display_name(&label);
        }
        apps.push(AppGroup {
            key,
            label,
            unit,
            cgroup,
            memory,
            swap,
            processes,
            executable,
            status: "Не управляется".to_string(),
            ..Default::default()
        });
    }
    apps.sort_by_key(|a| std::cmp::Reverse(a.memory));
    Ok(Snapshot {
        memory: memory_from_text(&meminfo, &psi),
        apps,
        sampled_at: crate::model::now(),
        ..Default::default()
    })
}

pub fn read_once() -> Result<Snapshot> {
    // getuid is read-only and has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut snapshot = collect(uid).context("Не удалось прочитать статистику памяти")?;
    snapshot.platform = crate::platform::detect();
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_survives_relaunch_but_does_not_cross_users() {
        assert_eq!(
            identity("app-gnome-firefox-123.scope").0,
            identity("app-gnome-firefox-987.scope").0
        );
        assert_eq!(
            identity("snap.firefox.firefox-22a9334b-35db-4f1c-a908-f25b005dea6b.scope").0,
            "app:snap.firefox.firefox"
        );
        assert_eq!(
            identity("app-gnome-jetbrains\\x2dtoolbox-42.scope").0,
            "app:app-gnome-jetbrains-toolbox"
        );
        assert!(
            managed_group(
                "/user.slice/user-1001.slice/user@1001.service/app.slice/x.scope",
                1000
            )
            .is_none()
        );
        assert!(
            managed_group(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/../x.scope",
                1000
            )
            .is_none()
        );
        assert_eq!(
            managed_group(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope/child",
                1000
            )
            .unwrap()
            .1,
            "x.scope"
        );
    }
    #[test]
    fn memory_measures_available_not_just_free() {
        let m = memory_from_text(
            "MemTotal: 1000 kB\nMemFree: 50 kB\nMemAvailable: 300 kB\nSwapTotal: 100 kB\nSwapFree: 80 kB",
            "some avg10=3.21 avg60=0.00\nfull avg10=1.50 avg60=0.00",
        );
        assert_eq!(m.available, 300 * 1024);
        assert_eq!(m.swap_used, 20 * 1024);
        assert_eq!(m.pressure_full_avg10, 1.5);
    }
}
