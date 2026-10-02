#!/usr/bin/env python3
"""Verify persisted language, localized tray and GUI refresh on an isolated agent."""
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


def call(method, signature='', *args):
    command = ['busctl', '--user', '--json=short', 'call', BUS, '/io/github/stabilizer/Agent', 'io.github.stabilizer.Agent1', method]
    if signature:
        command += [signature, *args]
    raw = run(*command)
    return json.loads(raw) if raw else None


def snapshot():
    return json.loads(call('Snapshot')['data'][0])


def wait_for(predicate):
    for _ in range(80):
        try:
            value = snapshot()
            if predicate(value):
                return value
        except subprocess.CalledProcessError:
            pass
        time.sleep(.2)
    raise AssertionError('Timed out waiting for locale state')


def menu_text():
    pid = int(run('systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'))
    bus = next(line.split()[0] for line in run('busctl', '--user', '--no-pager', '--no-legend', 'list').splitlines() if line.startswith(f'org.kde.StatusNotifierItem-{pid}-'))
    path = json.loads(run('busctl', '--user', '--json=short', 'get-property', bus, '/StatusNotifierItem', 'org.kde.StatusNotifierItem', 'Menu'))['data']
    return json.dumps(json.loads(run('busctl', '--user', '--json=short', 'call', bus, path, 'com.canonical.dbusmenu', 'GetLayout', 'iias', '0', '1', '0')), ensure_ascii=False)


def main():
    names = run('busctl', '--user', '--no-pager', '--no-legend', 'list')
    assert not any(line.split()[0] == BUS for line in names.splitlines()), 'An agent already exists'
    assert run('systemctl', '--user', 'show', UNIT, '-p', 'LoadState', '--value') == 'not-found'
    state = pathlib.Path(tempfile.mkdtemp(prefix='language-smoke-', dir=ROOT / 'work'))
    binary = ROOT / 'target/release/stabilizer-agent'
    gui = None
    try:
        run('systemd-run', '--user', '--collect', '--unit', UNIT,
            '--property=Type=dbus', '--property=BusName=io.github.stabilizer.Agent',
            '--property=Restart=always', '--property=RestartSec=2s',
            '--property=ManagedOOMPreference=omit', '--property=NoNewPrivileges=yes',
            f'--setenv=XDG_STATE_HOME={state}', str(binary))
        wait_for(lambda s: s['sampled_at'] > 0)
        for code, expected in [('en', 'Open Stabilizer'), ('ru', 'Открыть Stabilizer'), ('es', 'Abrir Stabilizer')]:
            call('SetLocale', 's', code)
            wait_for(lambda s: s['locale'] == code and s['sampled_at'] > 0)
            assert expected in menu_text()
        assert json.loads((state / 'stabilizer/state.json').read_text())['locale'] == 'es'
        print('PASS: English, Russian and Spanish are persisted and applied to the live tray')
        try:
            call('SetLocale', 's', 'unknown')
            raise AssertionError('Unsupported locale was accepted')
        except subprocess.CalledProcessError:
            pass
        assert snapshot()['locale'] == 'es'
        print('PASS: unsupported locale does not modify preferences')
        run('systemctl', '--user', 'restart', UNIT)
        wait_for(lambda s: s['locale'] == 'es' and s['sampled_at'] > 0)
        assert 'Abrir Stabilizer' in menu_text()
        print('PASS: agent restart preserves the language')
        call('SetLocale', 's', 'en')
        gui = subprocess.Popen([ROOT / 'target/release/stabilizer', '--screenshot', state / 'gui-refresh-es.png'],
                               env=dict(os.environ, XDG_STATE_HOME=str(state), GSK_RENDERER='cairo'))
        time.sleep(.2)
        call('SetLocale', 's', 'es')
        gui.wait(timeout=20)
        assert (state / 'gui-refresh-es.png').is_file()
        print('PASS: running native GUI refreshes after an agent language change')
        gui = subprocess.Popen([ROOT / 'target/release/stabilizer'], env=dict(os.environ, XDG_STATE_HOME=str(state), GSK_RENDERER='cairo'))
        time.sleep(3)
        run('systemctl', '--user', 'stop', UNIT)
        time.sleep(5)
        assert run('systemctl', '--user', 'show', UNIT, '-p', 'ActiveState', '--value') == 'inactive'
        assert not any(line.split()[0] == BUS for line in run('busctl', '--user', '--no-pager', '--no-legend', 'list').splitlines())
        print('PASS: intentional owner stop remains effective while the GUI is open')
        print(f'Evidence: {state}')
    finally:
        if gui and gui.poll() is None:
            gui.send_signal(signal.SIGTERM)
            gui.wait(timeout=10)
        subprocess.run(['systemctl', '--user', 'stop', UNIT], capture_output=True, timeout=15)


if __name__ == '__main__':
    main()
