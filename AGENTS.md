# Stabilizer

- Project name and binary names use ASCII `stabilizer`.
- Version 0.1 supports Ubuntu 26.04, Linux >= 7.0, systemd >= 259, cgroups v2 only.
- Native Rust + GTK4/libadwaita UI. Keep I/O and D-Bus calls off the GTK thread.
- User-session agent only; no permanent root daemon or direct process killing in v0.1.
- Protection means systemd-oomd protection. Never claim protection against the kernel OOM killer or all termination causes.
- Use cgroup totals, stable application identities and InvocationID checks. Preferences are not recursive.
- Preserve existing systemd values and external changes. Save undo data before mutating systemd.
- Check `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked --no-default-features`.
- `scripts/live-smoke.py` changes only its own temporary test service. Never stress the host memory or apply tests to GNOME/D-Bus.
- Package through `scripts/build-deb.sh`. Do not install the package or change the real session's protection as part of a build.
- The agent owns the SNI tray; closing the GUI must not stop policies or the tray. Use a dedicated user unit with `ManagedOOMPreference=omit` and `Restart=always` for self-protection; preserve intentional owner stop/uninstall.
- License is source-available with no resale. Do not describe it as OSI-approved Open Source or replace it with MIT/GPL without explicit owner authorization. Preserve third-party notices.
- Keep intermediate files in `work/`, packages in `dist/`; both are ignored by Git.

- Keep English, Spanish and Russian catalogs in sync, including placeholder order. Locale changes must not change application/rule identities. Test language persistence and GUI refresh with `scripts/language-smoke.py`.
