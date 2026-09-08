# Exact supplemental measurement workload and ordering.
# Run from the repository root after saving the baseline binary as
# target/release/weavepy-perf-base. Results go to tmp/performance-20260908.
import json, os, statistics, subprocess, sys, tempfile, time
from pathlib import Path

Path("tmp/performance-20260908").mkdir(parents=True, exist_ok=True)
base='target/release/weavepy-perf-base'; new='target/release/weavepy'

def measure(binary, command, cache=None):
    env=os.environ.copy()
    if cache is not None: env['WEAVEPY_STDLIB_CACHE']=cache
    with tempfile.TemporaryFile() as out:
        start=time.perf_counter_ns()
        proc=subprocess.Popen([binary,*command],env=env,stdout=out,stderr=subprocess.STDOUT)
        _,status,usage=os.wait4(proc.pid,0)
        elapsed=time.perf_counter_ns()-start; proc.returncode=os.waitstatus_to_exitcode(status)
        out.seek(0); output=out.read().decode()
    assert proc.returncode==0,output
    return {'wall_ns':elapsed,'cpu_ns':round((usage.ru_utime+usage.ru_stime)*1e9),'rss_bytes':usage.ru_maxrss,'output':output}

rows={}
for name,command in [('pass',['-c','pass']),('no_site',['-S','-c','pass']),('imports',['-c','import json, datetime, collections, pathlib'])]:
    samples={label:[] for label in ('base','new','cpython')}
    for run in range(32):
        variants=[('base',base),('new',new),('cpython','python3.14')]
        for label,binary in variants if run%2==0 else reversed(variants):
            value=measure(binary,command)
            if run: samples[label].append(value)
    rows[name]=samples
    print(name,{label:round(statistics.median(v['wall_ns'] for v in values)/1e6,2) for label,values in samples.items()},flush=True)
rows['cold_tree']={label:[] for label in ('base','new')}
for run in range(7):
    for label,binary in [('base',base),('new',new)] if run%2==0 else [('new',new),('base',base)]:
        with tempfile.TemporaryDirectory(prefix='weavepy-cold-') as cache:
            rows['cold_tree'][label].append(measure(binary,['-c','pass'],cache))
print('cold_tree',{label:round(statistics.median(v['wall_ns'] for v in values)/1e6,2) for label,values in rows['cold_tree'].items()},flush=True)
Path('tmp/performance-20260908/startup.json').write_text(json.dumps(rows,indent=2)+'\n')
