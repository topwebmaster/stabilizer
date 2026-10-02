use serde::{Deserialize, Serialize};
use std::{fs, process::Command};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub kernel: String,
    pub supported: bool,
    pub reason: String,
}

pub fn kernel_supported(release: &str) -> bool {
    let mut parts = release.split(['.', '-']);
    matches!(
        (parts.next().and_then(|s| s.parse::<u32>().ok()), parts.next().and_then(|s| s.parse::<u32>().ok())),
        (Some(major), Some(_)) if major >= 7
    )
}

pub fn detect() -> Platform {
    let os_release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| -> String {
        os_release
            .lines()
            .find_map(|line| {
                line.strip_prefix(&format!("{key}="))
                    .map(|s| s.trim_matches('"').to_string())
            })
            .unwrap_or_default()
    };
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .trim()
        .to_string();
    let systemd_version = Command::new("systemctl")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u32>().ok());
    let reason = if field("ID") != "ubuntu" || field("VERSION_ID") != "26.04" {
        "Первая версия поддерживает только Ubuntu 26.04".to_string()
    } else if !kernel_supported(&kernel) {
        "Требуется ядро Linux 7.0 или новее".to_string()
    } else if !std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        "Требуется cgroups v2".to_string()
    } else if systemd_version.is_none_or(|v| v < 259) {
        "Требуется systemd 259 или новее".to_string()
    } else {
        String::new()
    };
    Platform {
        os: field("PRETTY_NAME"),
        kernel,
        supported: reason.is_empty(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_rejects_older_and_malformed_kernels() {
        for release in ["6.19.1", "garbage", "7", ""] {
            assert!(!super::kernel_supported(release));
        }
        for release in ["7.0.0-38-generic", "7.1.0", "8.0.1"] {
            assert!(super::kernel_supported(release));
        }
    }
}
