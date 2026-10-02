#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"
mkdir -p work dist
python3 - <<'PY'
import os, pathlib
fields = dict(line.split('=', 1) for line in pathlib.Path('/etc/os-release').read_text().splitlines() if '=' in line)
assert fields.get('ID', '').strip('"') == 'ubuntu' and fields.get('VERSION_ID', '').strip('"') == '26.04', 'Build on Ubuntu 26.04'
if os.environ.get('STABILIZER_CROSS_BUILD') != '1':
    assert int(pathlib.Path('/proc/sys/kernel/osrelease').read_text().split('.')[0]) >= 7, 'Linux >= 7.0 required'
PY
cargo build --release --locked -j "${STABILIZER_BUILD_JOBS:-2}"
stabilizer_target_dir="${CARGO_TARGET_DIR:-$project_dir/target}"
case "$stabilizer_target_dir" in /*) ;; *) stabilizer_target_dir="$project_dir/$stabilizer_target_dir" ;; esac
package_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)"
package_arch="$(dpkg --print-architecture)"
stage_dir="$(mktemp -d "$project_dir/work/deb-stage.XXXXXX")"
# Keep staging directories for inspection; never recursively clean the user's workspace.
install -Dm755 "$stabilizer_target_dir/release/stabilizer" "$stage_dir/usr/bin/stabilizer"
install -Dm755 "$stabilizer_target_dir/release/stabilizer-agent" "$stage_dir/usr/bin/stabilizer-agent"
install -Dm644 data/io.github.stabilizer.Stabilizer.desktop "$stage_dir/usr/share/applications/io.github.stabilizer.Stabilizer.desktop"
install -Dm644 data/io.github.stabilizer.Stabilizer.svg "$stage_dir/usr/share/icons/hicolor/scalable/apps/io.github.stabilizer.Stabilizer.svg"
install -Dm644 data/io.github.stabilizer.Agent.service "$stage_dir/usr/share/dbus-1/services/io.github.stabilizer.Agent.service"
install -Dm644 data/stabilizer-agent.service "$stage_dir/usr/lib/systemd/user/stabilizer-agent.service"
install -Dm644 README.md "$stage_dir/usr/share/doc/stabilizer/README.md"
install -Dm644 README.ru.md "$stage_dir/usr/share/doc/stabilizer/README.ru.md"
install -Dm644 LICENSE "$stage_dir/usr/share/doc/stabilizer/copyright"
install -Dm644 THIRD_PARTY_NOTICES.md "$stage_dir/usr/share/doc/stabilizer/THIRD_PARTY_NOTICES.md"
mkdir -p "$stage_dir/usr/lib/systemd/user/default.target.wants" "$stage_dir/DEBIAN" dist
ln -s ../stabilizer-agent.service "$stage_dir/usr/lib/systemd/user/default.target.wants/stabilizer-agent.service"
installed_size="$(du -sk "$stage_dir/usr" | cut -f1)"
cat > "$stage_dir/DEBIAN/control" <<EOF
Package: stabilizer
Version: $package_version
Section: utils
Priority: optional
Architecture: $package_arch
Maintainer: Stabilizer Project <stabilizer@localhost>
Installed-Size: $installed_size
Depends: libc6 (>= 2.42), libgcc-s1, libgtk-4-1 (>= 4.18), libadwaita-1-0 (>= 1.7), libglib2.0-0t64, dbus-user-session, systemd (>= 259), systemd-oomd (>= 259)
Description: Native memory monitor and systemd-oomd policy manager
 Supports Ubuntu 26.04 with Linux 7.0 or later and cgroups v2.
 User-session agent with persistent application priorities and reversible rules.
EOF
cat > "$stage_dir/DEBIAN/preinst" <<'EOF'
#!/bin/sh
set -eu
if [ "$1" = install ] || [ "$1" = upgrade ]; then
    . /etc/os-release
    if [ "$ID" != ubuntu ] || [ "$VERSION_ID" != 26.04 ]; then
        echo "Stabilizer 0.1 supports Ubuntu 26.04 only." >&2
        exit 1
    fi
    kernel_major=$(uname -r | cut -d. -f1)
    if [ "$kernel_major" -lt 7 ] || [ ! -f /sys/fs/cgroup/cgroup.controllers ]; then
        echo "Stabilizer requires Linux >= 7.0 and cgroups v2." >&2
        exit 1
    fi
fi
EOF
chmod 755 "$stage_dir/DEBIAN/preinst"
find "$stage_dir" -type d -exec chmod 755 {} +
(
    cd "$stage_dir"
    find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums
)
package_path="dist/stabilizer_${package_version}_${package_arch}.deb"
dpkg-deb --root-owner-group --build "$stage_dir" "$package_path"
(
    cd dist
    sha256sum "$(basename "$package_path")" > "$(basename "$package_path").sha256"
)
printf 'Built %s\n' "$project_dir/$package_path"
