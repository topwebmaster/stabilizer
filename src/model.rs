use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Protected,
    High,
    #[default]
    Normal,
}

impl Priority {
    pub fn preference(self) -> &'static str {
        match self {
            Self::Protected => "omit",
            Self::High => "avoid",
            Self::Normal => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Protected => "Защищённое",
            Self::High => "Высокий приоритет",
            Self::Normal => "Обычное",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub key: String,
    pub label: String,
    pub priority: Priority,
    pub memory_high_mib: Option<u64>,
}

impl Rule {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.key.len() <= 512
                && (self.key.starts_with("app:") || self.key.starts_with("service:"))
                && !self.key.chars().any(char::is_control),
            "Неверный идентификатор приложения"
        );
        anyhow::ensure!(
            !self.label.is_empty() && self.label.len() <= 256,
            "Неверное название приложения"
        );
        if let Some(mib) = self.memory_high_mib {
            anyhow::ensure!(
                (128..=1_048_576).contains(&mib),
                "Лимит должен быть от 128 до 1048576 МиБ"
            );
            anyhow::ensure!(
                self.priority != Priority::Protected,
                "Защищённому приложению нельзя задавать мягкий лимит: он создаёт давление на память"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub pressure_some_avg10: f64,
    pub pressure_full_avg10: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppGroup {
    pub key: String,
    pub label: String,
    pub unit: String,
    pub cgroup: String,
    pub memory: u64,
    pub swap: u64,
    pub processes: usize,
    pub executable: String,
    pub observed_preference: Option<String>,
    pub observed_high: Option<u64>,
    pub status: String,
    pub protected_effective: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Event {
    pub unix_time: u64,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub platform: crate::platform::Platform,
    pub memory: Memory,
    pub apps: Vec<AppGroup>,
    pub rules: Vec<Rule>,
    pub events: Vec<Event>,
    pub sampled_at: u64,
    pub error: Option<String>,
    pub engine_status: String,
    pub self_protection: SelfProtection,
    pub tray_online: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelfProtection {
    pub oomd: bool,
    pub auto_restart: bool,
    pub detail: String,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn bytes_label(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} ГиБ", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.0} МиБ", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_rules_cannot_create_reclaim_pressure() {
        let mut rule = Rule {
            key: "service:dbus.service".into(),
            label: "D-Bus".into(),
            priority: Priority::Protected,
            memory_high_mib: Some(512),
        };
        assert!(rule.validate().is_err());
        rule.memory_high_mib = None;
        assert!(rule.validate().is_ok());
        rule.key = "../root.service".into();
        assert!(rule.validate().is_err());
    }
}
