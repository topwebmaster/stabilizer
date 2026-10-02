#!/usr/bin/env python3
"""Integration checks on a disposable user service; never stress the host's RAM."""
import argparse
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parent.parent
(ROOT / 'work').mkdir(exist_ok=True)
BUS = 'io.github.stabilizer.Agent'
OBJECT = '/io/github/stabilizer/Agent'
INTERFACE = 'io.github.stabilizer.Agent1'


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.PIPE, timeout=15).strip()


def call(method, signature='', *args):
    command = ['busctl', '--user', '--json=short', 'call', BUS, OBJECT, INTERFACE, method]
    if signature:
        command.extend([signature, *args])
    result = run(*command)
    return json.loads(result) if result else None


def snapshot():
    return json.loads(call('Snapshot')['data'][0])


def preference(unit):
    return run('systemctl', '--user', 'show', unit, '-p', 'ManagedOOMPreference', '-p', 'MemoryHigh')


def wait_for(predicate):
    last = None
    for _ in range(60):
        try:
            last = snapshot()
            if predicate(last):
                return last
        except (subprocess.CalledProcessError, KeyError):
            pass
        time.sleep(.2)
    raise AssertionError(f'Timed out; snapshot={last}')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--release', action='store_true')
    args = parser.parse_args()
    # Do not replace or interrupt an agent already managing the real session.
    names = run('busctl', '--user', '--no-pager', '--no-legend', 'list')
    assert not any(line.split()[0] == BUS for line in names.splitlines()), 'Stop your development agent before running the test'
    binary = ROOT / 'target' / ('release' if args.release else 'debug') / 'stabilizer-agent'
    state_dir = pathlib.Path(tempfile.mkdtemp(prefix='smoke-', dir=ROOT / 'work'))
    env = dict(os.environ, XDG_STATE_HOME=str(state_dir))
    unit = f'stabilizer-smoke-{uuid.uuid4().hex[:10]}.service'
    key = f'service:{unit}'
    critical_before = {u: preference(u) for u in ['dbus.service', 'org.gnome.Shell@ubuntu.service']}
    log = open(state_dir / 'agent.log', 'w')
    agent = None
    try:
        run('systemd-run', '--user', '--collect', '--unit', unit, '/usr/bin/sleep', '180')
        baseline = preference(unit)
        agent = subprocess.Popen([binary], env=env, stdout=log, stderr=log)
        initial = wait_for(lambda s: any(a['unit'] == unit for a in s['apps']))
        assert initial['platform']['supported'], initial['platform']

        def save(priority, high=None):
            call('SetRule', 's', json.dumps(dict(key=key, label='Disposable smoke service', priority=priority, memory_high_mib=high)))
            return wait_for(lambda s: any(a['unit'] == unit and a['observed_preference'] == {'protected': 'omit', 'high': 'avoid', 'normal': 'none'}[priority] for a in s['apps']))

        protected = save('protected')
        row = next(a for a in protected['apps'] if a['unit'] == unit)
        assert row['protected_effective'], row
        assert 'ManagedOOMPreference=omit' in preference(unit)
        print('PASS: oomd protection applied and verified')

        # Invalid policy must not replace a working rule.
        try:
            save('protected', 512)
            raise AssertionError('Invalid protected + MemoryHigh rule was accepted')
        except subprocess.CalledProcessError:
            pass
        assert 'ManagedOOMPreference=omit' in preference(unit)
        print('PASS: invalid rule rejected without changing the existing policy')

        save('high', 512)
        assert 'MemoryHigh=536870912' in preference(unit)
        print('PASS: avoid priority and soft memory limit applied')

        # The rules and original settings must survive an agent restart.
        agent.send_signal(signal.SIGINT)
        agent.wait(timeout=10)
        agent = subprocess.Popen([binary], env=env, stdout=log, stderr=log)
        wait_for(lambda s: any(a['unit'] == unit and a['observed_preference'] == 'avoid' for a in s['apps']))
        call('RemoveRule', 's', key)
        assert preference(unit) == baseline
        print('PASS: agent restart preserved reversible policy')

        save('protected')
        run('systemctl', '--user', 'stop', unit)
        wait_for(lambda s: all(a['unit'] != unit for a in s['apps']))
        run('systemd-run', '--user', '--collect', '--unit', unit, '/usr/bin/sleep', '180')
        wait_for(lambda s: any(a['unit'] == unit and a['observed_preference'] == 'omit' for a in s['apps']))
        call('RemoveRule', 's', key)
        assert preference(unit) == baseline
        print('PASS: rule reapplied after application restart; removal restored baseline')

        save('protected')
        run('systemctl', '--user', 'set-property', '--runtime', unit, 'ManagedOOMPreference=avoid')
        call('RemoveRule', 's', key)
        assert 'ManagedOOMPreference=avoid' in preference(unit)
        print('PASS: removing a rule preserves subsequent external changes')

        critical_after = {u: preference(u) for u in critical_before}
        assert critical_before == critical_after
        print('PASS: D-Bus and GNOME settings unchanged')
        print(f'Evidence: {state_dir}')
    finally:
        if agent and agent.poll() is None:
            agent.send_signal(signal.SIGINT)
            agent.wait(timeout=10)
        subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True, timeout=15)
        log.close()


if __name__ == '__main__':
    main()
