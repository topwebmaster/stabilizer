# Contributing

Issues and pull requests are welcome. Please describe the problem, expected behavior,
Ubuntu/kernel/systemd versions, and the relevant cgroup structure. Avoid posting
private process arguments, session logs, tokens or personal paths.

The initial supported platform is Ubuntu 26.04 with Linux >= 7.0 and cgroups v2.
Keep GTK work on the main thread, I/O outside it, and mutations serialized by the
agent. Do not introduce a permanent root daemon, independent process killer,
unbounded logging, or claims that omit/avoid protects against all termination.

Run the format, Clippy and unit checks listed in the README. Test policy changes
only on disposable user services; never use host memory exhaustion as a routine
test. Build packages with `scripts/build-deb.sh`.

Contributions are accepted under the repository's existing no-resale license.
By submitting a contribution you confirm that you have the necessary rights and
agree that it may be distributed under that license. Third-party material must
keep its original notices and compatible licensing.
