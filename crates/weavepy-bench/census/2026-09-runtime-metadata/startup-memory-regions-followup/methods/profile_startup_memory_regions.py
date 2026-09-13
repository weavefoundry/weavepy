"""Inspect startup memory regions; these snapshots are neither peak RSS nor timing."""
import argparse,datetime,hashlib,json,os,selectors,shutil,subprocess
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',required=True);p.add_argument('--reference',required=True);p.add_argument('--out',type=Path,required=True)
a=p.parse_args()
assert os.environ.get('WEAVEPY_BENCH_LAUNCH_CONTEXT')=='outside-tool-filesystem-sandbox (require_escalated)'
assert not a.out.exists();a.out.mkdir()
driver=a.out/'driver.py'
driver.write_text('import os, time\nprint(os.getpid(), 17 * 19, flush=True)\ntime.sleep(120)\n')
variants=[('cpython',shutil.which('python3.14')),('reference',a.reference),('new',a.binary)]
report={'purpose':__doc__,'driver_sha256':hashlib.sha256(driver.read_bytes()).hexdigest(),'instrumentation':'ps RSS snapshot and vmmap -summary after identical PID/checksum readiness marker','rows':{},'limitations':'This is an instrumented, point-in-time region diagnostic, not an uninstrumented peak-RSS or performance comparison.'}
def cache_snapshot(directory):
    return {str(p.relative_to(directory)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(directory.rglob('*')) if p.is_file()}

for label,binary in variants:
    digest=hashlib.sha256(Path(binary).read_bytes()).hexdigest()
    env=dict(os.environ)
    for key in ['WEAVEPY_JIT','WEAVEPY_GIL','WEAVEPY_VM_STATS','WEAVEPY_JIT_TRACE','WEAVEPY_JIT_THRESHOLD','MallocStackLogging','MallocStackLoggingNoCompact']:
        env.pop(key,None)
    if label!='cpython':env.update(WEAVEPY_JIT='1',WEAVEPY_STDLIB_CACHE=str(Path('target/performance-stdlib-cache').resolve()),WEAVEPY_FROZEN_CACHE=str((a.out/'frozen'/digest).resolve()))
    cache_dir=a.out/'frozen'/digest
    row=report['rows'][label]={'binary':binary,'binary_sha256':digest,'runs':[]}
    # One declared unmeasured seed process creates any frozen artifacts first.
    for stage in ['seed','inspect']:
        child=subprocess.Popen([binary,str(driver)],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        run={'stage':stage,'pid':child.pid,'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'load':list(os.getloadavg())};row['runs'].append(run)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout,selectors.EVENT_READ)
                assert selector.select(45),'Process did not announce readiness.'
            line=child.stdout.readline();run['readiness']=line
            fields=line.split();assert len(fields)==2 and int(fields[0])==child.pid and int(fields[1])==323,line
            if stage=='inspect':
                for name,command in [('rss',['/bin/ps','-o','rss=','-p',str(child.pid)]),('vmmap',['/usr/bin/vmmap','-summary',str(child.pid)])]:
                    result=subprocess.run(command,text=True,capture_output=True,timeout=55)
                    (a.out/(label+'-'+name+'.txt')).write_text(result.stdout)
                    (a.out/(label+'-'+name+'-stderr.txt')).write_text(result.stderr)
                    run[name]={'command':command,'returncode':result.returncode,'stdout_sha256':hashlib.sha256(result.stdout.encode()).hexdigest()}
                    if name=='rss' and result.returncode==0:run['rss_snapshot_bytes']=int(result.stdout.strip())*1024
                # Keep platform access failures as observations, without silently
                # retrying or presenting a partial map as a complete comparison.
                print(label,run.get('rss_snapshot_bytes'),run.get('vmmap'),flush=True)
        finally:
            if child.poll() is None:child.terminate()
            try:_,errors=child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill();_,errors=child.communicate(timeout=5)
            (a.out/(label+'-'+stage+'-stderr.txt')).write_text(errors)
            run['child_returncode_after_owned_termination']=child.returncode
            run['frozen_cache_after']=cache_snapshot(cache_dir) if label!='cpython' else None
            (a.out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    row['frozen_cache_unchanged_after_seed']=row['runs'][0]['frozen_cache_after']==row['runs'][1]['frozen_cache_after']
report['status']='complete';(a.out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
