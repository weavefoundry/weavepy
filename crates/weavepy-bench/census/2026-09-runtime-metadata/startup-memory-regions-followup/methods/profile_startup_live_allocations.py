"""Capture one startup live-allocation trace to locate heap costs, not churn or RSS."""
import gzip,hashlib,json,os,selectors,subprocess
from pathlib import Path
root=Path('target/binder-flags-startup-live-allocations');assert not root.exists();root.mkdir()
binary=Path('target/release/weavepy-runtime-binder-flags').resolve()
driver=root/'driver.py';driver.write_bytes(Path('target/binder-flags-startup-memory-regions/driver.py').read_bytes())
logs=root/'stacklogs';logs.mkdir()
env=dict(os.environ)
for key in ['WEAVEPY_GIL','WEAVEPY_JIT_TRACE','WEAVEPY_VM_STATS','WEAVEPY_JIT_THRESHOLD','MallocStackLoggingNoCompact']:env.pop(key,None)
env.update(WEAVEPY_JIT='1',WEAVEPY_STDLIB_CACHE=str(Path('target/performance-stdlib-cache').resolve()),WEAVEPY_FROZEN_CACHE=str((root/'frozen').resolve()),MallocStackLogging='1',MallocStackLoggingDirectory=str(logs.resolve()))
report={'purpose':__doc__,'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'driver_sha256':hashlib.sha256(driver.read_bytes()).hexdigest(),'load':list(os.getloadavg()),'instrumentation':'MallocStackLogging=1; malloc_history -allBySize -fullStacks','limitations':'Instrumented live allocation sites only; no CPython object-count, allocator-churn, requested-byte, RSS, or timing claim.'}
child=subprocess.Popen([str(binary),str(driver)],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
try:
    with selectors.DefaultSelector() as selector:
        selector.register(child.stdout,selectors.EVENT_READ)
        assert selector.select(45),'Child did not announce readiness.'
    line=child.stdout.readline();report['readiness']=line
    fields=line.split();assert len(fields)==2 and int(fields[0])==child.pid and int(fields[1])==323,line
    command=['/usr/bin/malloc_history',str(child.pid),'-allBySize','-fullStacks']
    result=subprocess.run(command,capture_output=True,timeout=55)
    (root/'history.txt.gz').write_bytes(gzip.compress(result.stdout,mtime=0));(root/'history-stderr.txt').write_bytes(result.stderr)
    report.update(command=command,returncode=result.returncode,history_bytes=len(result.stdout),history_sha256=hashlib.sha256(result.stdout).hexdigest())
    assert result.returncode==0,result.stderr.decode(errors='replace')
    print('Captured',len(result.stdout),'bytes of instrumented live-allocation stacks.',flush=True)
finally:
    if child.poll() is None:child.terminate()
    try:_,errors=child.communicate(timeout=5)
    except subprocess.TimeoutExpired:child.kill();_,errors=child.communicate(timeout=5)
    (root/'child-stderr.txt').write_text(errors);report['owned_child_exit']=child.returncode
    (root/'report.json').write_text(json.dumps(report,indent=2)+'\n')
