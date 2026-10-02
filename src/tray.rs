use crate::{
    agent::Command,
    model::{Snapshot, bytes_label},
};
use ksni::{MenuItem, ToolTip, Tray, menu::StandardItem};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayView {
    pub available: String,
    pub used: String,
    pub swap: String,
    pub pressure: String,
    pub protected: String,
    pub self_protection: String,
}

impl TrayView {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut view = Self {
            available: format!("Доступно RAM: {}", bytes_label(snapshot.memory.available)),
            used: format!(
                "Занято RAM: {} / {}",
                bytes_label(
                    snapshot
                        .memory
                        .total
                        .saturating_sub(snapshot.memory.available)
                ),
                bytes_label(snapshot.memory.total)
            ),
            swap: format!("Swap: {}", bytes_label(snapshot.memory.swap_used)),
            pressure: format!(
                "Давление памяти: {:.1}%",
                snapshot.memory.pressure_full_avg10
            ),
            protected: format!(
                "Защищено групп: {}",
                snapshot
                    .apps
                    .iter()
                    .filter(|a| a.protected_effective)
                    .count()
            ),
            self_protection: format!(
                "Агент: {}",
                if snapshot.self_protection.oomd && snapshot.self_protection.auto_restart {
                    "защищён · автоперезапуск"
                } else {
                    &snapshot.self_protection.detail
                }
            ),
        };
        view.available = crate::i18n::t(&view.available);
        view.used = crate::i18n::t(&view.used);
        view.swap = crate::i18n::t(&view.swap);
        view.pressure = crate::i18n::t(&view.pressure);
        view.protected = crate::i18n::t(&view.protected);
        view.self_protection = crate::i18n::t(&view.self_protection);
        view
    }
}

pub struct MemoryTray {
    pub view: TrayView,
    sender: mpsc::Sender<Command>,
    online: Arc<AtomicBool>,
}

impl MemoryTray {
    pub(crate) fn new(sender: mpsc::Sender<Command>, online: Arc<AtomicBool>) -> Self {
        Self {
            view: TrayView::default(),
            sender,
            online,
        }
    }
}

impl Tray for MemoryTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "stabilizer".into()
    }
    fn title(&self) -> String {
        "Stabilizer".into()
    }
    fn icon_name(&self) -> String {
        "io.github.stabilizer.Stabilizer".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // Native fallback for development builds before the themed SVG is installed.
        let mut pixels = Vec::with_capacity(32 * 32 * 4);
        for y in 0_i32..32 {
            for x in 0_i32..32 {
                let shield = (7..25).contains(&x) && (6..19).contains(&y)
                    || ((19..27).contains(&y) && (x - 16).abs() < 27 - y);
                let pulse = (11..22).contains(&x) && (y - (16 + (x - 16).abs() / 2)).abs() <= 1;
                let color = if shield && !pulse {
                    [255, 255, 255, 255]
                } else {
                    [255, 28, 113, 216]
                };
                pixels.extend_from_slice(&color);
            }
        }
        vec![ksni::Icon {
            width: 32,
            height: 32,
            data: pixels,
        }]
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Stabilizer".into(),
            description: format!(
                "{}\n{}\n{}",
                self.view.available, self.view.swap, self.view.self_protection
            ),
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let info = |text: &str| {
            StandardItem {
                label: if text.is_empty() {
                    crate::i18n::t("Подключение…")
                } else {
                    text.into()
                },
                enabled: false,
                ..Default::default()
            }
            .into()
        };
        vec![
            info(&self.view.available),
            info(&self.view.used),
            info(&self.view.swap),
            info(&self.view.pressure),
            info(&self.view.protected),
            MenuItem::Separator,
            info(&self.view.self_protection),
            MenuItem::Separator,
            StandardItem {
                label: crate::i18n::t("Открыть Stabilizer"),
                icon_name: "view-grid-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.try_send(Command::OpenGui);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        let _ = self.sender.try_send(Command::OpenGui);
    }
    fn watcher_online(&self) {
        self.online.store(true, Ordering::Relaxed);
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.online.store(false, Ordering::Relaxed);
        true
    }
}

pub async fn open_gui() -> anyhow::Result<()> {
    let path = std::env::current_exe()?.with_file_name("stabilizer");
    let mut child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reap short-lived secondary launches as well as the main window without blocking the agent.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tray_does_not_claim_self_protection_when_unverified() {
        let snapshot = Snapshot::default();
        let view = TrayView::from_snapshot(&snapshot);
        assert!(!view.self_protection.contains("защищён"));
        assert_eq!(view.protected, crate::i18n::t("Защищено групп: 0"));
    }
}
