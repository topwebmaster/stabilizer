#!/usr/bin/env python3
"""Exercise SNI tray and crash recovery using only a dedicated development unit."""
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
(ROOT / 'work').mkdir(exist_ok=True)
BUS = 'io.github.stabilizer.Agent'
UNIT = 'stabilizer-agent-dev.service'


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.PIPE, timeout=15).strip()


def snapshot():
    reply = json.loads(run('busctl', '--user', '--json=short', 'call', BUS, '/io/github/stabilizer/Agent', 'io.github.stabilizer.Agent1', 'Snapshot'))
    return json.loads(reply['data'][0])


def wait_for(predicate):
    for _ in range(80):
        try:
            value = snapshot()
            if predicate(value):
                return value
        except (subprocess.CalledProcessError, KeyError):
            pass
        time.sleep(.2)
    raise AssertionError('Agent did not reach the requested state')


def main():
    names = run('busctl', '--user', '--no-pager', '--no-legend', 'list')
    assert not any(line.split()[0] == BUS for line in names.splitlines()), 'An existing agent is running; do not interrupt it'
    assert run('systemctl', '--user', 'show', UNIT, '-p', 'LoadState', '--value') == 'not-found', 'Development unit already exists'
    state = pathlib.Path(tempfile.mkdtemp(prefix='tray-smoke-', dir=ROOT / 'work'))
    binary = ROOT / 'target/release/stabilizer-agent'
    critical_before = run('systemctl', '--user', 'show', 'dbus.service', 'org.gnome.Shell@ubuntu.service', '-p', 'ManagedOOMPreference', '-p', 'MemoryHigh')
    try:
        run('systemd-run', '--user', '--collect', '--unit', UNIT,
            '--property=Type=dbus', '--property=BusName=io.github.stabilizer.Agent',
            '--property=Restart=always', '--property=RestartSec=2s',
            '--property=ManagedOOMPreference=omit', '--property=NoNewPrivileges=yes',
            f'--setenv=XDG_STATE_HOME={state}', str(binary))
        first = wait_for(lambda s: s['sampled_at'] > 0 and s['self_protection']['oomd'] and s['self_protection']['auto_restart'])
        assert first['tray_online'], 'SNI watcher did not register the tray'
        pid = int(run('systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'))
        tray_bus = next(line.split()[0] for line in run('busctl', '--user', '--no-pager', '--no-legend', 'list').splitlines() if line.startswith(f'org.kde.StatusNotifierItem-{pid}-'))
        title = run('busctl', '--user', 'get-property', tray_bus, '/StatusNotifierItem', 'org.kde.StatusNotifierItem', 'Title')
        assert 'Stabilizer' in title
        is_menu = run('busctl', '--user', 'get-property', tray_bus, '/StatusNotifierItem', 'org.kde.StatusNotifierItem', 'ItemIsMenu')
        assert is_menu == 'b true', is_menu
        menu_path = json.loads(run('busctl', '--user', '--json=short', 'get-property', tray_bus, '/StatusNotifierItem', 'org.kde.StatusNotifierItem', 'Menu'))['data']
        menu = json.loads(run('busctl', '--user', '--json=short', 'call', tray_bus, menu_path, 'com.canonical.dbusmenu', 'GetLayout', 'iias', '0', '1', '0'))
        text = json.dumps(menu, ensure_ascii=False)
        for expected in ['Доступно RAM', 'Занято RAM', 'Swap', 'Давление памяти', 'Защищено групп', 'Открыть Stabilizer']:
            assert expected in text, expected
        (state / 'tray-menu.json').write_text(json.dumps(menu, ensure_ascii=False, indent=2))
        print('PASS: native tray registered; click menu exposes live memory and protection data')
        assert os.readlink(f'/proc/{pid}/exe') == str(binary)
        os.kill(pid, signal.SIGKILL)  # Only the process started by this test.
        second = wait_for(lambda s: s['sampled_at'] > first['sampled_at'] and s['self_protection']['oomd'])
        new_pid = int(run('systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'))
        assert new_pid != pid
        assert second['tray_online']
        print('PASS: agent and tray recovered automatically after forced termination')
        assert critical_before == run('systemctl', '--user', 'show', 'dbus.service', 'org.gnome.Shell@ubuntu.service', '-p', 'ManagedOOMPreference', '-p', 'MemoryHigh')
        print('PASS: real D-Bus and GNOME policies unchanged')
        print(f'Evidence: {state}')
    finally:
        subprocess.run(['systemctl', '--user', 'stop', UNIT], capture_output=True, timeout=15)


if __name__ == '__main__':
    main()
