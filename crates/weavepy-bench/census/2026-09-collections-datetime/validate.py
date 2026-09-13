import json, os, pathlib, subprocess, time
root=pathlib.Path.cwd()
output=root/'crates/weavepy-bench/census/2026-09-collections-datetime/validation.json'
os.environ['WEAVEPY_STDLIB_CACHE']=str(root/'target/performance-stdlib-cache')
results=[]
binary=str(root/'target/release/weavepy')
def run(name, cmd, *, env=None, expected=None, timeout=180):
    start=time.monotonic()
    try:
        p=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,env=env,timeout=timeout)
        passed=p.returncode==0 and (expected is None or p.stdout.replace('\r\n','\n')==expected)
        record=dict(name=name,passed=passed,returncode=p.returncode,seconds=time.monotonic()-start,stdout=p.stdout[-5000:],stderr=p.stderr[-5000:])
    except subprocess.TimeoutExpired:
        record=dict(name=name,passed=False,timeout=True,seconds=time.monotonic()-start)
    results.append(record)
    output.write_text(json.dumps(results,indent=2)+'\n')
    print(name,'PASS' if record['passed'] else 'FAIL',flush=True)
for name,flags,jit in [('jit',[],'1'),('interp',[],'0'),('gil0',['-X','gil=0'],'1')]:
    run('collections_datetime_'+name,[binary,*flags,'tests/regrtest/test_collections_datetime_fastpaths.py'],env={**os.environ,'WEAVEPY_JIT':jit})
    run('json_buffers_'+name,[binary,*flags,'tests/regrtest/test_json_buffers.py'],env={**os.environ,'WEAVEPY_JIT':jit})
    run('string_fastpaths_'+name,[binary,*flags,'tests/regrtest/test_string_fastpaths.py'],env={**os.environ,'WEAVEPY_JIT':jit})
run('cpython_datetime_oracles',['python3.14','crates/weavepy-bench/census/2026-09-collections-datetime/verify.py','--weavepy',binary,'--base',str(root/'target/release/weavepy-perf-4177e85'),'--out',str(root/'crates/weavepy-bench/census/2026-09-collections-datetime/oracles.json')])
run('gil0_runtime',[binary,'tests/regrtest/test_rfc0076_gil0.py'])
run('collections',[binary,'tests/regrtest/test_collections.py'])
run('queue_pingpong',[binary,'tests/regrtest/test_ws3_queue_pingpong.py'])
run('cpython_json_oracles',['python3.14','crates/weavepy-bench/census/2026-09-json/verify.py','--weavepy',binary])
for p in sorted((root/'crates/weavepy/tests/fixtures/run').glob('*.py')):
    if p.name.startswith('_'):continue
    source=p.read_text()
    if any('weavepy-skip:' in line and 'macos' in line for line in source.splitlines()[:20]):continue
    run('fixture/'+p.name,[binary,str(p)],expected=p.with_suffix('.out').read_text(),timeout=90)
for label in ['test_datetime','test_deque','test_collections','test_queue','test_json','test_scope','test_frame','test_exceptions','test_generators','test_sys_settrace','test_sys_setprofile','test_gc','test_str','test_userstring']:
    run('cpython/'+label,['target/release/weavepy-conformance','regrtest','--mode','subprocess','--weavepy',binary,'--filter','cpython/Lib/test/'+label,'--stream'],timeout=900)
print('PASSED',sum(r['passed'] for r in results),'OF',len(results),flush=True)
raise SystemExit(any(not r['passed'] for r in results))
