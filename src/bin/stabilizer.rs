use adw::prelude::*;
use gtk::{gio, glib};
use stabilizer::{
    BUS_NAME, INTERFACE, OBJECT_PATH,
    model::{AppGroup, Priority, Rule, Snapshot, bytes_label},
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, sync::mpsc, time::Duration};

use stabilizer::i18n::{self, Locale, t};

thread_local! { static GUI_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

enum Request {
    Set(Rule),
    Remove(String),
    Protect,
    Locale(Locale),
}
enum Update {
    Snapshot(Box<Snapshot>),
    Error(String),
    Done,
    LocaleChanged,
}

fn worker(tx: mpsc::Sender<Update>, rx: mpsc::Receiver<Request>) {
    let result = (|| -> anyhow::Result<()> {
        let connection = zbus::blocking::Connection::session()?;
        let proxy = zbus::blocking::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)?;
        let mut started = false;
        loop {
            let request = match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(r) => Some(r),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            };
            if let Some(request) = request {
                let changing_locale = matches!(request, Request::Locale(_));
                let outcome = match request {
                    Request::Set(rule) => serde_json::to_string(&rule)
                        .map_err(anyhow::Error::from)
                        .and_then(|json| {
                            proxy
                                .call::<_, _, ()>("SetRule", &(json,))
                                .map_err(Into::into)
                        }),
                    Request::Remove(key) => proxy
                        .call::<_, _, ()>("RemoveRule", &(key,))
                        .map_err(Into::into),
                    Request::Locale(locale) => proxy
                        .call::<_, _, ()>("SetLocale", &(locale.code(),))
                        .map_err(Into::into),
                    Request::Protect => proxy
                        .call::<_, _, ()>("ProtectSession", &())
                        .map_err(Into::into),
                };
                let _ = tx.send(match outcome {
                    Ok(()) if changing_locale => Update::LocaleChanged,
                    Ok(()) => Update::Done,
                    Err(e) => Update::Error(e.to_string()),
                });
            }
            let response = if started {
                proxy
                    .call_with_flags::<_, _, String>(
                        "Snapshot",
                        zbus::proxy::MethodFlags::NoAutoStart.into(),
                        &(),
                    )
                    .and_then(|reply| {
                        reply.ok_or_else(|| zbus::Error::Failure("Missing reply".into()))
                    })
            } else {
                proxy.call::<_, _, String>("Snapshot", &())
            };
            match response {
                Ok(json) => {
                    started = true;
                    match serde_json::from_str(&json) {
                        Ok(snapshot) => {
                            if tx.send(Update::Snapshot(Box::new(snapshot))).is_err() {
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Update::Error(format!("Ошибка ответа агента: {e}")));
                        }
                    }
                }
                Err(e) => {
                    if !started {
                        started = true;
                        let sibling = std::env::current_exe()?.with_file_name("stabilizer-agent");
                        if sibling.is_file() {
                            let mut start = std::process::Command::new("systemd-run");
                            start.args([
                                "--user",
                                "--collect",
                                "--unit=stabilizer-agent-dev.service",
                                "--property=Type=dbus",
                                "--property=BusName=io.github.stabilizer.Agent",
                                "--property=Restart=always",
                                "--property=RestartSec=2s",
                                "--property=ManagedOOMPreference=omit",
                                "--property=NoNewPrivileges=yes",
                            ]);
                            if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
                                start.arg(format!(
                                    "--setenv=XDG_STATE_HOME={}",
                                    path.to_string_lossy()
                                ));
                            }
                            start
                                .arg(sibling)
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .status()?;
                            continue;
                        }
                    }
                    let _ = tx.send(Update::Error(format!("Нет связи с фоновым агентом: {e}")));
                }
            }
        }
    })();
    if let Err(e) = result {
        let _ = tx.send(Update::Error(e.to_string()));
    }
}

#[derive(Clone)]
struct Row {
    widget: gtk::ListBoxRow,
    name: gtk::Label,
    detail: gtk::Label,
    memory: gtk::Label,
    badge: gtk::Label,
    status: gtk::Label,
}

fn label(text: &str, style: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(&t(text)));
    label.set_xalign(0.0);
    if !style.is_empty() {
        label.add_css_class(style);
    }
    label
}

fn set_text_if_changed(label: &gtk::Label, text: &str) {
    let text = t(text);
    if label.text().as_str() != text {
        label.set_text(&text);
    }
}

fn localized_dropdown(strings: &[&str]) -> gtk::DropDown {
    let labels: Vec<String> = strings.iter().map(|s| t(s)).collect();
    gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>())
}

fn metric(title: &str) -> (gtk::Box, gtk::Label) {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
    box_.add_css_class("card");
    box_.add_css_class("metric");
    box_.set_hexpand(true);
    let heading = label(title, "dim-label");
    heading.set_wrap(true);
    box_.append(&heading);
    let value = label("—", "title-2");
    box_.append(&value);
    (box_, value)
}

fn edit_rule(
    window: &adw::ApplicationWindow,
    overlay: &adw::ToastOverlay,
    rule: Rule,
    existing: bool,
    sender: mpsc::Sender<Request>,
) {
    let dialog = adw::Dialog::builder()
        .title(t("Правило приложения"))
        .content_width(480)
        .build();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.append(&label(&rule.label, "title-2"));
    let explanation = label(
        "Приоритет действует при выборе приложения для завершения systemd-oomd.",
        "dim-label",
    );
    explanation.set_wrap(true);
    body.append(&explanation);
    let group = adw::PreferencesGroup::new();
    let dropdown = localized_dropdown(&["Защищённое", "Высокий приоритет", "Обычное"]);
    dropdown.set_selected(match rule.priority {
        Priority::Protected => 0,
        Priority::High => 1,
        Priority::Normal => 2,
    });
    let priority_row = adw::ActionRow::builder()
        .title(t("Приоритет памяти"))
        .build();
    priority_row.add_suffix(&dropdown);
    group.add(&priority_row);
    let limit = gtk::SpinButton::with_range(0.0, 1_048_576.0, 128.0);
    limit.set_value(rule.memory_high_mib.unwrap_or(0) as f64);
    limit.set_sensitive(rule.priority != Priority::Protected);
    let limit_row = adw::ActionRow::builder()
        .title(t("Мягкий лимит RAM, МиБ"))
        .subtitle(t("0 — исходный лимит приложения"))
        .build();
    limit_row.add_suffix(&limit);
    group.add(&limit_row);
    let note = label(
        "Мягкий лимит вызывает освобождение памяти и может замедлить приложение. Защита относится к oomd; завершение ядром или другим приложением — отдельные механизмы.",
        "dim-label",
    );
    note.set_wrap(true);
    body.append(&group);
    body.append(&note);
    let limit_clone = limit.clone();
    dropdown.connect_selected_notify(move |d| {
        limit_clone.set_sensitive(d.selected() != 0);
        if d.selected() == 0 {
            limit_clone.set_value(0.0);
        }
    });
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    if existing {
        let remove = gtk::Button::with_label(&t("Удалить правило"));
        remove.add_css_class("destructive-action");
        let key = rule.key.clone();
        let s = sender.clone();
        let d = dialog.clone();
        remove.connect_clicked(move |_| {
            let _ = s.send(Request::Remove(key.clone()));
            d.close();
        });
        actions.append(&remove);
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    let save = gtk::Button::with_label(&t("Сохранить"));
    save.add_css_class("suggested-action");
    let d = dialog.clone();
    let toast = overlay.clone();
    save.connect_clicked(move |_| {
        let mut updated = rule.clone();
        updated.priority = match dropdown.selected() {
            0 => Priority::Protected,
            1 => Priority::High,
            _ => Priority::Normal,
        };
        let mib = limit.value() as u64;
        updated.memory_high_mib = (mib > 0).then_some(mib);
        match updated.validate() {
            Ok(()) => {
                let _ = sender.send(Request::Set(updated));
                d.close();
            }
            Err(e) => toast.add_toast(adw::Toast::new(&e.to_string())),
        }
    });
    actions.append(&save);
    body.append(&actions);
    page.append(&body);
    dialog.set_child(Some(&page));
    dialog.present(Some(window));
}

fn make_row(
    app: &AppGroup,
    window: &adw::ApplicationWindow,
    overlay: &adw::ToastOverlay,
    state: &Rc<RefCell<Snapshot>>,
    sender: &mpsc::Sender<Request>,
    demo: bool,
) -> Row {
    let widget = gtk::ListBoxRow::new();
    widget.set_selectable(false);
    widget.set_activatable(false);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    let icon = gtk::Image::from_icon_name(if app.key.starts_with("service:") {
        "system-run-symbolic"
    } else {
        "application-x-executable-symbolic"
    });
    icon.set_pixel_size(26);
    icon.add_css_class("dim-label");
    content.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let name = label(&app.label, "heading");
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let detail = label("", "dim-label");
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let status = label("", "caption");
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&name);
    text.append(&detail);
    text.append(&status);
    content.append(&text);
    let badge = label("", "badge");
    badge.set_valign(gtk::Align::Center);
    content.append(&badge);
    let memory = label("", "monospace");
    memory.set_width_chars(10);
    memory.set_xalign(1.0);
    content.append(&memory);
    let edit = gtk::Button::from_icon_name("emblem-system-symbolic");
    edit.add_css_class("flat");
    edit.set_tooltip_text(Some(&t("Настроить правило")));
    edit.set_sensitive(!demo && state.borrow().platform.supported);
    let app_clone = app.clone();
    let window = window.clone();
    let overlay = overlay.clone();
    let state = state.clone();
    let sender = sender.clone();
    edit.connect_clicked(move |_| {
        let state = state.borrow();
        let existing = state.rules.iter().find(|r| r.key == app_clone.key);
        let rule = existing.cloned().unwrap_or(Rule {
            key: app_clone.key.clone(),
            label: app_clone.label.clone(),
            priority: Priority::Normal,
            memory_high_mib: None,
        });
        edit_rule(&window, &overlay, rule, existing.is_some(), sender.clone());
    });
    content.append(&edit);
    widget.set_child(Some(&content));
    Row {
        widget,
        name,
        detail,
        memory,
        badge,
        status,
    }
}

fn demo_snapshot() -> Snapshot {
    let mut snapshot = Snapshot {
        locale: i18n::locale(),
        platform: stabilizer::platform::Platform {
            os: "Ubuntu 26.04".into(),
            kernel: "7.0.0".into(),
            supported: true,
            reason: String::new(),
        },
        engine_status: "Демонстрация · системные настройки не изменяются".into(),
        ..Default::default()
    };
    snapshot.self_protection = stabilizer::model::SelfProtection {
        oomd: true,
        auto_restart: true,
        detail: "Демонстрация самозащиты".into(),
    };
    snapshot.memory = stabilizer::model::Memory {
        total: 40 * 1024_u64.pow(3),
        available: 25 * 1024_u64.pow(3),
        swap_total: 48 * 1024_u64.pow(3),
        swap_used: 1024_u64.pow(3),
        pressure_some_avg10: 0.2,
        pressure_full_avg10: 0.0,
    };
    for (unit, mib, count, priority) in [
        ("app-gnome-idea-123.scope", 4096, 18, Some(Priority::High)),
        ("snap.firefox.firefox-123.scope", 2560, 32, None),
        (
            "org.gnome.Shell@ubuntu.service",
            650,
            6,
            Some(Priority::Protected),
        ),
        (
            "app-flatpak-com.ktechpit.whatsie-123.scope",
            512,
            5,
            Some(Priority::Normal),
        ),
        ("dbus.service", 24, 2, Some(Priority::Protected)),
    ] {
        let (key, mut title) = stabilizer::monitor::identity(unit);
        title = match title.as_str() {
            "idea" => "IntelliJ IDEA".into(),
            "snap.firefox.firefox" => "Firefox".into(),
            "com.ktechpit.whatsie" => "Whatsie".into(),
            _ => title,
        };
        let mut app = AppGroup {
            key: key.clone(),
            label: title.clone(),
            unit: unit.into(),
            memory: mib * 1024 * 1024,
            processes: count,
            status: "Не управляется".into(),
            ..Default::default()
        };
        if let Some(priority) = priority {
            snapshot.rules.push(Rule {
                key,
                label: title,
                priority,
                memory_high_mib: None,
            });
            app.status = format!("Применено · {}", priority.label());
            app.protected_effective = priority == Priority::Protected;
        }
        snapshot.apps.push(app);
    }
    snapshot
}

fn build_ui(application: &adw::Application, demo: bool, screenshot: Option<PathBuf>) {
    let generation = GUI_GENERATION.with(|value| {
        value.set(value.get() + 1);
        value.get()
    });
    if generation == 1 {
        let css = gtk::CssProvider::new();
        css.load_from_string(".metric { padding: 18px; } .badge { font-size: 12px; padding: 5px 10px; border-radius: 8px; background: alpha(@accent_color, 0.10); } .protected { color: @success_color; } .page { padding: 20px; } .error-status { color: @error_color; }");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("GTK display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let window = application
        .active_window()
        .and_then(|window| window.downcast::<adw::ApplicationWindow>().ok())
        .unwrap_or_else(|| {
            adw::ApplicationWindow::builder()
                .application(application)
                .title("Stabilizer")
                .default_width(1060)
                .default_height(780)
                .build()
        });
    let state = Rc::new(RefCell::new(Snapshot::default()));
    let rows: Rc<RefCell<HashMap<String, Row>>> = Rc::new(RefCell::new(HashMap::new()));
    let (sender, requests) = mpsc::channel();
    let (updates, receiver) = mpsc::channel();
    if demo {
        let _ = updates.send(Update::Snapshot(Box::new(demo_snapshot())));
    } else {
        std::thread::spawn(move || worker(updates, requests));
    }
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let language = gtk::DropDown::from_strings(&["English", "Español", "Русский"]);
    language.set_selected(match i18n::locale() {
        Locale::En => 0,
        Locale::Es => 1,
        Locale::Ru => 2,
    });
    language.set_tooltip_text(Some(&t("Язык")));
    let language_sender = sender.clone();
    let language_app = application.clone();
    language.connect_selected_notify(move |dropdown| {
        let locale = match dropdown.selected() {
            1 => Locale::Es,
            2 => Locale::Ru,
            _ => Locale::En,
        };
        if demo {
            i18n::set_locale(locale);
            build_ui(&language_app, true, None);
        } else {
            let _ = language_sender.send(Request::Locale(locale));
        }
    });
    header.pack_end(&language);
    let title = adw::WindowTitle::new("Stabilizer", &t("Память под вашим контролем"));
    header.set_title_widget(Some(&title));
    let about = gtk::Button::from_icon_name("help-about-symbolic");
    about.set_tooltip_text(Some(&t("О приложении")));
    let parent = window.clone();
    about.connect_clicked(move |_| {
        let dialog = adw::AboutDialog::builder().application_name("Stabilizer").application_icon("io.github.stabilizer.Stabilizer").version(env!("CARGO_PKG_VERSION"))
            .comments(t("Монитор памяти и правил systemd-oomd. Ubuntu 26.04 · Linux ≥ 7.0. Защита относится к oomd, а не ко всем причинам завершения процесса."))
            .license_type(gtk::License::Custom).license(include_str!("../../LICENSE"))
            .copyright("© 2026 Dmitriy Petrov (topwebmaster)").website("https://github.com/topwebmaster/stabilizer")
            .build(); dialog.present(Some(&parent));
    });
    header.pack_end(&about);
    layout.append(&header);
    let overlay = adw::ToastOverlay::new();
    overlay.set_vexpand(true);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.add_css_class("page");
    let metrics = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let (ram_card, ram) = metric("Доступно RAM");
    metrics.append(&ram_card);
    let (usage_card, usage) = metric("Занято RAM");
    metrics.append(&usage_card);
    let (swap_card, swap) = metric("Используется swap");
    metrics.append(&swap_card);
    let (psi_card, psi) = metric("Давление памяти · 10 с");
    metrics.append(&psi_card);
    body.append(&metrics);
    let progress = gtk::ProgressBar::new();
    body.append(&progress);
    let policy = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let status = label("Подключение к фоновому агенту…", "dim-label");
    status.set_wrap(true);
    status.set_hexpand(true);
    policy.append(&status);
    let protect = gtk::Button::with_label(&t("Защитить сеанс"));
    protect.set_sensitive(false);
    protect.add_css_class("suggested-action");
    protect.set_tooltip_text(Some(&t(
        "Сохранить защиту D-Bus и GNOME Shell от systemd-oomd",
    )));
    let send = sender.clone();
    protect.connect_clicked(move |b| {
        b.set_sensitive(false);
        let _ = send.send(Request::Protect);
    });
    policy.append(&protect);
    body.append(&policy);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_halign(gtk::Align::Start);
    body.append(&switcher);
    let monitor_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&t("Найти приложение или службу")));
    monitor_page.append(&search);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    monitor_page.append(&scroller);
    let note = label(
        "Память учитывается для всей группы процессов. Лимиты и защита применяются в фоне каждые 2 секунды.",
        "caption",
    );
    note.set_wrap(true);
    monitor_page.append(&note);
    stack.add_titled(&monitor_page, Some("monitor"), &t("Приложения"));
    let rules_list = gtk::ListBox::new();
    rules_list.add_css_class("boxed-list");
    rules_list.set_selection_mode(gtk::SelectionMode::None);
    let rules_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&rules_list)
        .build();
    stack.add_titled(&rules_page, Some("rules"), &t("Правила"));
    let events = gtk::TextView::new();
    events.set_editable(false);
    events.set_cursor_visible(false);
    events.set_wrap_mode(gtk::WrapMode::WordChar);
    events.set_top_margin(12);
    events.set_left_margin(12);
    events.set_right_margin(12);
    let events_page = gtk::ScrolledWindow::builder().child(&events).build();
    stack.add_titled(&events_page, Some("events"), &t("Журнал"));
    body.append(&stack);
    let footer = label(
        "Ubuntu 26.04 · Linux ≥ 7.0 · защита от systemd-oomd",
        "caption",
    );
    body.append(&footer);
    overlay.set_child(Some(&body));
    layout.append(&overlay);
    window.set_content(Some(&layout));
    let rows_search = rows.clone();
    search.connect_search_changed(move |search| {
        let needle = search.text().to_lowercase();
        for row in rows_search.borrow().values() {
            row.widget.set_visible(
                row.name.text().to_lowercase().contains(&needle)
                    || row.detail.text().to_lowercase().contains(&needle),
            );
        }
    });
    let last_rules = Rc::new(RefCell::new(String::new()));
    let mut captured = false;
    let mut last_error = String::new();
    let mut last_events = String::new();
    let app = application.clone();
    let win = window.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        if GUI_GENERATION.with(|value| value.get()) != generation {
            return glib::ControlFlow::Break;
        }
        for update in receiver.try_iter() {
            match update {
                Update::Error(error) => {
                    set_text_if_changed(&status, &error);
                    if last_error != error {
                        overlay.add_toast(adw::Toast::new(&t(&error)));
                        last_error = error;
                    }
                    protect.set_sensitive(false);
                    set_text_if_changed(&footer, "Самозащита не подтверждена");
                }
                Update::LocaleChanged => {}
                Update::Done => overlay.add_toast(adw::Toast::new(&t(
                    "Правила сохранены. Результат применения показан в списке.",
                ))),
                Update::Snapshot(snapshot) => {
                    last_error.clear();
                    if !demo && snapshot.sampled_at > 0 && snapshot.locale != i18n::locale() {
                        i18n::set_locale(snapshot.locale);
                        build_ui(&app, false, screenshot.clone());
                        return glib::ControlFlow::Break;
                    }
                    if let Some(error) = &snapshot.error {
                        set_text_if_changed(&status, error);
                    } else if !snapshot.platform.supported {
                        set_text_if_changed(
                            &status,
                            &format!("Только просмотр: {}", snapshot.platform.reason),
                        );
                    } else {
                        set_text_if_changed(&status, &snapshot.engine_status);
                    }
                    protect.set_sensitive(snapshot.platform.supported && !demo);
                    set_text_if_changed(&ram, &bytes_label(snapshot.memory.available));
                    set_text_if_changed(
                        &usage,
                        &format!(
                            "{} / {}",
                            bytes_label(
                                snapshot
                                    .memory
                                    .total
                                    .saturating_sub(snapshot.memory.available)
                            ),
                            bytes_label(snapshot.memory.total)
                        ),
                    );
                    set_text_if_changed(&swap, &bytes_label(snapshot.memory.swap_used));
                    set_text_if_changed(
                        &psi,
                        &format!("{:.1}%", snapshot.memory.pressure_full_avg10),
                    );
                    psi.set_tooltip_text(Some(&t("PSI full: доля времени, когда все выполняемые задачи ожидали память. Это не процент занятой RAM.")));
                    if snapshot.memory.total > 0 {
                        progress.set_fraction(
                            1.0 - snapshot.memory.available as f64 / snapshot.memory.total as f64,
                        );
                    }
                    set_text_if_changed(
                        &footer,
                        &format!(
                            "{} · Linux {} · {} групп · {}",
                            snapshot.platform.os,
                            snapshot.platform.kernel,
                            snapshot.apps.len(),
                            if snapshot.self_protection.oomd
                                && snapshot.self_protection.auto_restart
                            {
                                "Самозащита активна"
                            } else {
                                "Самозащита не подтверждена"
                            }
                        ),
                    );
                    footer.set_tooltip_text(Some(&snapshot.self_protection.detail));
                    *state.borrow_mut() = (*snapshot).clone();
                    let active: std::collections::HashSet<_> =
                        snapshot.apps.iter().map(|a| a.unit.as_str()).collect();
                    rows.borrow_mut().retain(|unit, row| {
                        if active.contains(unit.as_str()) {
                            true
                        } else {
                            list.remove(&row.widget);
                            false
                        }
                    });
                    for app in &snapshot.apps {
                        let mut rows = rows.borrow_mut();
                        let row = rows.entry(app.unit.clone()).or_insert_with(|| {
                            let row = make_row(app, &win, &overlay, &state, &sender, demo);
                            list.append(&row.widget);
                            row
                        });
                        set_text_if_changed(&row.name, &app.label);
                        row.name.set_tooltip_text(Some(&app.executable));
                        set_text_if_changed(
                            &row.detail,
                            &format!(
                                "{} процессов · swap {}",
                                app.processes,
                                bytes_label(app.swap)
                            ),
                        );
                        set_text_if_changed(&row.memory, &bytes_label(app.memory));
                        set_text_if_changed(&row.status, &app.status);
                        row.status
                            .set_tooltip_text(Some(&format!("{}\n{}", app.unit, app.status)));
                        let rule = snapshot.rules.iter().find(|r| r.key == app.key);
                        set_text_if_changed(
                            &row.badge,
                            rule.map(|r| r.priority.label()).unwrap_or("Без правила"),
                        );
                        if app.protected_effective {
                            row.badge.add_css_class("protected");
                        } else {
                            row.badge.remove_css_class("protected");
                        }
                        let needle = search.text().to_lowercase();
                        row.widget.set_visible(
                            app.label.to_lowercase().contains(&needle)
                                || row.detail.text().to_lowercase().contains(&needle),
                        );
                    }
                    let rules_json = serde_json::to_string(&snapshot.rules).unwrap_or_default();
                    if *last_rules.borrow() != rules_json {
                        *last_rules.borrow_mut() = rules_json;
                        while let Some(child) = rules_list.first_child() {
                            rules_list.remove(&child);
                        }
                        if snapshot.rules.is_empty() {
                            let empty = adw::ActionRow::builder()
                                .title(t("Правил пока нет"))
                                .subtitle(
                                    "Откройте настройки приложения или нажмите «Защитить сеанс».",
                                )
                                .build();
                            rules_list.append(&empty);
                        }
                        for rule in &snapshot.rules {
                            let row = adw::ActionRow::builder()
                                .title(t(&rule.label))
                                .subtitle(t(&format!(
                                    "{} · {}",
                                    rule.priority.label(),
                                    rule.memory_high_mib
                                        .map(|m| format!("лимит {m} МиБ"))
                                        .unwrap_or("без нового лимита".into())
                                )))
                                .build();
                            let edit = gtk::Button::from_icon_name("emblem-system-symbolic");
                            edit.set_valign(gtk::Align::Center);
                            edit.add_css_class("flat");
                            edit.set_sensitive(snapshot.platform.supported && !demo);
                            let rule = rule.clone();
                            let win = win.clone();
                            let toast = overlay.clone();
                            let tx = sender.clone();
                            edit.connect_clicked(move |_| {
                                edit_rule(&win, &toast, rule.clone(), true, tx.clone())
                            });
                            row.add_suffix(&edit);
                            rules_list.append(&row);
                        }
                    }
                    let lines = snapshot
                        .events
                        .iter()
                        .map(|e| {
                            let time = glib::DateTime::from_unix_local(e.unix_time as i64)
                                .ok()
                                .and_then(|d| d.format("%d.%m %H:%M:%S").ok())
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            format!("{time}  {}", e.message)
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let event_text = if lines.is_empty() {
                        "Здесь появится история применения правил."
                    } else {
                        &lines
                    };
                    if last_events != event_text {
                        events.buffer().set_text(&t(event_text));
                        last_events = event_text.to_string();
                    }
                    if !captured
                        && !snapshot.apps.is_empty()
                        && let Some(path) = &screenshot
                    {
                        captured = true;
                        let path = path.clone();
                        let win = win.clone();
                        let app = app.clone();
                        glib::timeout_add_local_once(Duration::from_millis(700), move || {
                            let paintable = gtk::WidgetPaintable::new(Some(&win));
                            let snapshot = gtk::Snapshot::new();
                            paintable.snapshot(&snapshot, win.width() as f64, win.height() as f64);
                            if let Some(node) = snapshot.to_node()
                                && let Some(renderer) = win.renderer()
                            {
                                let texture = renderer.render_texture(&node, None);
                                if let Err(e) = texture.save_to_png(&path) {
                                    eprintln!("Снимок не сохранён: {e}");
                                }
                            }
                            app.quit();
                        });
                    }
                }
            }
        }
        glib::ControlFlow::Continue
    });
    window.present();
}

fn main() -> glib::ExitCode {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == "--help") {
        println!(
            "stabilizer [--demo] [--screenshot /absolute/path.png]\nНативный монитор памяти и управление правилами systemd-oomd."
        );
        return glib::ExitCode::SUCCESS;
    }
    let stored = stabilizer::storage::path()
        .ok()
        .and_then(|path| stabilizer::storage::load(&path).ok())
        .map(|store| store.locale)
        .unwrap_or_else(Locale::system);
    let chosen = args
        .iter()
        .position(|s| s == "--lang")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| Locale::parse(s))
        .unwrap_or(stored);
    i18n::set_locale(chosen);
    // No worker threads or GTK objects exist yet; initialize gettext's process language once.
    unsafe {
        std::env::set_var("LANGUAGE", format!("{}:en", chosen.code()));
    }
    let demo = args.iter().any(|s| s == "--demo");
    let screenshot = args
        .iter()
        .position(|s| s == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let application = adw::Application::builder()
        .application_id("io.github.stabilizer.Stabilizer")
        .flags(if demo || screenshot.is_some() {
            gio::ApplicationFlags::NON_UNIQUE
        } else {
            gio::ApplicationFlags::empty()
        })
        .build();
    application.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            build_ui(app, demo, screenshot.clone());
        }
    });
    application.run_with_args(&["stabilizer"])
}
