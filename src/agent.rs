use crate::{
    BUS_NAME, OBJECT_PATH,
    model::{AppGroup, Priority, Rule, Snapshot},
    storage::{self, Store},
    systemd,
};
use anyhow::{Context, Result};
use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{RwLock, mpsc, oneshot};
use zbus::{Connection, fdo};

pub(crate) enum Command {
    Set(Rule, oneshot::Sender<Result<()>>),
    Remove(String, oneshot::Sender<Result<()>>),
    ProtectSession(oneshot::Sender<Result<()>>),
    OpenGui,
}

struct Interface {
    snapshot: Arc<RwLock<Snapshot>>,
    commands: mpsc::Sender<Command>,
}

fn dbus_error(e: impl std::fmt::Display) -> fdo::Error {
    fdo::Error::Failed(e.to_string())
}

#[zbus::interface(name = "io.github.stabilizer.Agent1")]
impl Interface {
    async fn snapshot(&self) -> fdo::Result<String> {
        serde_json::to_string(&*self.snapshot.read().await).map_err(dbus_error)
    }

    async fn set_rule(&self, json: &str) -> fdo::Result<()> {
        if json.len() > 4096 {
            return Err(fdo::Error::InvalidArgs("Правило слишком большое".into()));
        }
        let rule: Rule =
            serde_json::from_str(json).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        rule.validate()
            .map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Set(rule, tx))
            .await
            .map_err(dbus_error)?;
        rx.await.map_err(dbus_error)?.map_err(dbus_error)
    }

    async fn remove_rule(&self, key: &str) -> fdo::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Remove(key.to_string(), tx))
            .await
            .map_err(dbus_error)?;
        rx.await.map_err(dbus_error)?.map_err(dbus_error)
    }

    async fn protect_session(&self) -> fdo::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::ProtectSession(tx))
            .await
            .map_err(dbus_error)?;
        rx.await.map_err(dbus_error)?.map_err(dbus_error)
    }
}

struct Engine {
    user: Connection,
    system: Connection,
    uid: u32,
    path: PathBuf,
    store: Store,
    platform: crate::platform::Platform,
    snapshot: Arc<RwLock<Snapshot>>,
    tray: Option<ksni::Handle<crate::tray::MemoryTray>>,
    tray_online: Arc<std::sync::atomic::AtomicBool>,
}

impl Engine {
    async fn persist(&self) -> Result<()> {
        let path = self.path.clone();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || storage::save(&path, &store)).await??;
        Ok(())
    }

    async fn set_rule(&mut self, rule: Rule) -> Result<()> {
        anyhow::ensure!(self.platform.supported, "{}", self.platform.reason);
        rule.validate()?;
        let before = self.store.clone();
        self.store.rules.retain(|r| r.key != rule.key);
        self.store.event(format!(
            "Правило сохранено: {} → {}",
            rule.label,
            rule.priority.label()
        ));
        self.store.rules.push(rule);
        if let Err(e) = self.persist().await {
            self.store = before;
            return Err(e);
        }
        Ok(())
    }

    async fn remove_rule(&mut self, key: &str) -> Result<()> {
        anyhow::ensure!(self.platform.supported, "{}", self.platform.reason);
        let records: Vec<_> = self
            .store
            .applied
            .iter()
            .filter(|r| r.key == key)
            .cloned()
            .collect();
        // Restore before deleting the durable undo records. Failures leave the rule visible.
        for record in &records {
            if let Ok(observed) = systemd::live(&self.user, &record.unit).await {
                if observed.invocation != record.invocation {
                    continue;
                }
                match systemd::restoration(record, &observed) {
                    Ok(reset) => systemd::set(&self.user, &reset).await?,
                    Err(_) => self.store.event(format!(
                        "{}: внешние изменения сохранены при удалении правила",
                        record.unit
                    )),
                }
                self.store.applied.retain(|r| r.unit != record.unit);
                self.persist().await?;
            }
        }
        self.store.rules.retain(|r| r.key != key);
        self.store.applied.retain(|r| r.key != key);
        self.store.event(format!("Правило удалено: {key}"));
        self.persist().await
    }

    async fn tick(&mut self) -> Result<()> {
        let uid = self.uid;
        let mut sample =
            tokio::task::spawn_blocking(move || crate::monitor::collect(uid)).await??;
        sample.platform = self.platform.clone();
        let coverage = systemd::coverage(&self.system, uid).await;
        sample.engine_status = coverage.message.clone();
        sample.self_protection =
            systemd::self_protection(&self.user, self.uid, coverage.effective).await;
        sample.tray_online = self.tray_online.load(std::sync::atomic::Ordering::Relaxed);
        let units: HashSet<_> = sample.apps.iter().map(|a| a.unit.clone()).collect();
        let old_count = self.store.applied.len();
        self.store.applied.retain(|r| units.contains(&r.unit));
        let mut changed = old_count != self.store.applied.len();
        if self.platform.supported {
            for app in &mut sample.apps {
                let Some(rule) = self.store.rules.iter().find(|r| r.key == app.key).cloned() else {
                    continue;
                };
                match self.apply(app, &rule, coverage.effective).await {
                    Ok(did_change) => changed |= did_change,
                    Err(e) => {
                        app.status = format!("Не применено: {e}");
                        app.protected_effective = false;
                    }
                }
            }
        }
        if changed {
            self.persist().await?;
        }
        sample.rules = self.store.rules.clone();
        sample.events = self.store.events.iter().rev().take(30).cloned().collect();
        if let Some(tray) = &self.tray {
            let view = crate::tray::TrayView::from_snapshot(&sample);
            tray.update(move |tray| tray.view = view).await;
        }
        *self.snapshot.write().await = sample;
        Ok(())
    }

    async fn apply(&mut self, app: &mut AppGroup, rule: &Rule, coverage: bool) -> Result<bool> {
        let observed = systemd::live(&self.user, &app.unit).await?;
        systemd::target_is_safe(app, &observed, self.uid)?;
        let index = self
            .store
            .applied
            .iter()
            .position(|r| r.unit == app.unit && r.invocation == observed.invocation);
        let mut record = index
            .map(|i| self.store.applied[i].clone())
            .unwrap_or_else(|| systemd::prepare(app, &observed, rule));
        // Respect subsequent changes from other managers instead of fighting over the unit.
        if index.is_some()
            && (observed.preference != record.last_preference || observed.high != record.last_high)
        {
            // A pending record may have been written just before a crash, before SetUnitProperties ran.
            anyhow::ensure!(
                record.pending_preference.as_deref() == Some(observed.preference.as_str())
                    && record.pending_high == Some(observed.high),
                "Настройки изменены другим инструментом; удалите конфликт перед применением"
            );
        }
        record.last_preference = rule.priority.preference().to_string();
        record.last_high = rule
            .memory_high_mib
            .map(|m| m * 1024 * 1024)
            .unwrap_or(record.previous_high);
        let needs_change =
            observed.preference != record.last_preference || observed.high != record.last_high;
        let pending = record.pending_preference.is_some();
        if needs_change {
            record.pending_preference = Some(observed.preference.clone());
            record.pending_high = Some(observed.high);
        }
        if index.is_none() || needs_change {
            self.store.applied.retain(|r| r.unit != app.unit);
            self.store.applied.push(record.clone());
            // Write the undo information before mutating systemd.
            self.persist().await?;
        }
        let verified = if needs_change {
            systemd::set(&self.user, &record).await?;
            let result = systemd::live(&self.user, &app.unit).await?;
            anyhow::ensure!(
                result.invocation == record.invocation
                    && result.preference == record.last_preference
                    && result.high == record.last_high,
                "systemd не подтвердил применение правила"
            );
            self.store.event(format!(
                "Применено: {} · {}",
                app.label,
                rule.priority.label()
            ));
            result
        } else {
            observed
        };
        systemd::verify_kernel_settings(&verified.cgroup, &verified.preference, verified.high)?;
        app.observed_preference = Some(verified.preference);
        app.observed_high = Some(verified.high);
        app.protected_effective = rule.priority == Priority::Protected && coverage;
        app.status = if !coverage && rule.priority != Priority::Normal {
            "Настроено · действие защиты не подтверждено".to_string()
        } else {
            format!("Применено · {}", rule.priority.label())
        };
        if (needs_change || pending)
            && let Some(record) = self.store.applied.iter_mut().find(|r| r.unit == app.unit)
        {
            record.pending_preference = None;
            record.pending_high = None;
        }
        Ok(needs_change || pending)
    }
}

pub async fn run() -> Result<()> {
    anyhow::ensure!(
        unsafe { libc::geteuid() } != 0,
        "Агент запускается от обычного пользователя, не root"
    );
    let path = storage::path()?;
    let store = storage::load(&path)?;
    let platform = crate::platform::detect();
    let snapshot = Arc::new(RwLock::new(Snapshot {
        platform: platform.clone(),
        engine_status: "Подключение к systemd…".into(),
        ..Default::default()
    }));
    let (commands, mut receiver) = mpsc::channel(16);
    let user = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            Interface {
                snapshot: snapshot.clone(),
                commands: commands.clone(),
            },
        )?
        .build()
        .await?;
    let system = Connection::system()
        .await
        .context("Не удалось подключиться к системной D-Bus")?;
    use ksni::TrayMethods;
    let tray_online = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tray = crate::tray::MemoryTray::new(commands, tray_online.clone())
        .assume_sni_available(true)
        .spawn()
        .await
        .ok();
    if tray.is_some() {
        // ksni calls watcher_online only after reconnecting; check the initial registration too.
        if let Ok(watcher) = zbus::Proxy::new(
            &user,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )
        .await
            && let Ok(items) = watcher
                .get_property::<Vec<String>>("RegisteredStatusNotifierItems")
                .await
        {
            tray_online.store(
                items.iter().any(|s| {
                    s.starts_with(&format!(
                        "org.kde.StatusNotifierItem-{}-",
                        std::process::id()
                    ))
                }),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }
    let mut engine = Engine {
        user,
        system,
        uid: unsafe { libc::getuid() },
        path,
        store,
        platform,
        snapshot,
        tray,
        tray_online,
    };
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = timer.tick() => {
                if let Err(e) = engine.tick().await {
                    engine.snapshot.write().await.error = Some(format!("Ошибка мониторинга: {e:#}"));
                    eprintln!("{e:#}");
                }
            }
            Some(command) = receiver.recv() => {
                if matches!(command, Command::OpenGui) {
                    if let Err(e) = crate::tray::open_gui() { eprintln!("Не удалось открыть окно: {e}"); }
                    continue;
                }
                let (result, reply) = match command {
                    Command::Set(rule, tx) => (engine.set_rule(rule).await, tx),
                    Command::Remove(key, tx) => (engine.remove_rule(&key).await, tx),
                    Command::ProtectSession(tx) => {
                        let mut result = Ok(());
                        for unit in ["dbus.service", "org.gnome.Shell@ubuntu.service"] {
                            let (key, label) = crate::monitor::identity(unit);
                            if let Err(e) = engine.set_rule(Rule { key, label, priority: Priority::Protected, memory_high_mib: None }).await { result = Err(e); break; }
                        }
                        (result, tx)
                    },
                    Command::OpenGui => unreachable!(),
                };
                // Publish an updated, verified snapshot before acknowledging the command.
                let sampled = engine.tick().await;
                let _ = reply.send(result.and(sampled));
            }
        }
    }
    Ok(())
}
