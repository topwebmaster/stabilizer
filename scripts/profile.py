#!/usr/bin/env python3
"""Measure an already-running agent plus a disposable native GUI window."""
import argparse
import json
import os
import pathlib
import signal
import subprocess
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent


def read(pid):
    stat = pathlib.Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    ticks = int(stat[11]) + int(stat[12])
    values = {}
    for line in pathlib.Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines():
        if ':' in line:
            key, value = line.split(':', 1)
            if key in ('Rss', 'Pss'):
                values[key] = int(value.split()[0]) / 1024
    return ticks, values


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--unit', default='stabilizer-agent.service')
    args = parser.parse_args()
    (ROOT / 'work').mkdir(exist_ok=True)
    agent_pid = int(subprocess.check_output(['systemctl', '--user', 'show', args.unit, '-p', 'MainPID', '--value'], text=True))
    assert agent_pid > 0, 'Start the requested agent unit first'
    gui = subprocess.Popen([ROOT / 'target/release/stabilizer'], env=os.environ.copy())
    try:
        time.sleep(4)
        before = {name: read(pid)[0] for name, pid in [('agent', agent_pid), ('gui', gui.pid)]}
        start = time.monotonic()
        time.sleep(20)
        elapsed = time.monotonic() - start
        results = {}
        for name, pid in [('agent', agent_pid), ('gui', gui.pid)]:
            ticks, memory = read(pid)
            results[name] = dict(pid=pid, rss_mib=round(memory['Rss'], 2), pss_mib=round(memory['Pss'], 2),
                                 cpu_percent_one_core=round((ticks - before[name]) / os.sysconf('SC_CLK_TCK') / elapsed * 100, 2))
        results['sample_seconds'] = round(elapsed, 2)
        (ROOT / 'work/performance.json').write_text(json.dumps(results, indent=2))
        print(json.dumps(results, indent=2))
    finally:
        gui.send_signal(signal.SIGTERM)
        gui.wait(timeout=10)


if __name__ == '__main__':
    main()
