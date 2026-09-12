"""Wait for lower host load, then retain telemetry throughout one complete run.

Load averages are context, not proof of an otherwise idle host. Once the child
starts, retain every sample regardless of later load; never filter or retry it.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--out', type=Path, required=True)
p.add_argument('--max-wait-seconds', type=float, default=600)
p.add_argument('--interval-seconds', type=float, default=10)
p.add_argument('--consecutive', type=int, default=3)
p.add_argument('--max-load', type=float, default=(os.cpu_count() or 1) / 2)
p.add_argument('command', nargs=argparse.REMAINDER)
a = p.parse_args()
command = a.command[1:] if a.command[:1] == ['--'] else a.command
assert command and a.interval_seconds > 0 and a.consecutive > 0
assert a.max_wait_seconds >= 0 and a.max_load >= 0
assert not a.out.exists(), 'Use a fresh telemetry file.'
a.out.parent.mkdir(parents=True, exist_ok=True)
started = time.monotonic()
report = {
    'purpose': __doc__, 'command': command, 'logical_cpus': os.cpu_count(),
    'precondition': {'max_load_1_and_5_minutes': a.max_load,
                     'consecutive_observations': a.consecutive,
                     'interval_seconds': a.interval_seconds,
                     'max_wait_seconds': a.max_wait_seconds},
    'status': 'waiting', 'observations': [],
    'selection_policy': 'No sample exclusion or automatic benchmark retry.',
}

def save():
    pending = a.out.with_name(a.out.name + '.partial')
    pending.write_text(json.dumps(report, indent=2) + '\n')
    pending.replace(a.out)

def observe(phase):
    load = os.getloadavg()
    report['observations'].append({
        'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'elapsed_seconds': time.monotonic() - started,
        'phase': phase, 'load_1_5_15_minutes': list(load),
    })
    save()
    return max(load[:2]) <= a.max_load

qualified = 0
while True:
    qualified = qualified + 1 if observe('precondition') else 0
    if qualified >= a.consecutive:
        break
    remaining = a.max_wait_seconds - (time.monotonic() - started)
    if remaining <= 0:
        report['status'] = 'not_started'
        report['reason'] = 'Host-load precondition was not met before the deadline.'
        save()
        print(report['reason'], flush=True)
        raise SystemExit(75)
    time.sleep(min(a.interval_seconds, remaining))

report['status'] = 'running'
report['launch_observation'] = len(report['observations']) - 1
save()
print('Host-load precondition met; starting one complete run.', flush=True)
try:
    child = subprocess.Popen(command)
    report['child_pid'] = child.pid
    save()
    while True:
        try:
            rc = child.wait(timeout=a.interval_seconds)
            break
        except subprocess.TimeoutExpired:
            observe('command')
    observe('command_finished')
    report['returncode'] = rc
    report['status'] = 'complete' if rc == 0 else 'command_failed'
    save()
except Exception as error:
    report['status'] = 'launch_or_monitor_error'
    report['error'] = repr(error)
    save()
    raise
raise SystemExit(rc)
