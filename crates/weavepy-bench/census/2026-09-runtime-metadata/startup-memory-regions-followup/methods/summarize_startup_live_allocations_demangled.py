"""Attribute each malloc stack to its first demangled WeavePy implementation."""
import collections,gzip,hashlib,json,re,subprocess
from pathlib import Path
out=Path('target/binder-flags-startup-allocation-sites.json');assert not out.exists()
roots=[('cold',Path('target/binder-flags-startup-live-allocations')),('warm',Path('target/binder-flags-startup-live-allocations-warm'))]
header=re.compile(r'^(\d+) calls? for (\d+) bytes:')
frame=re.compile(r'^(\d+)\s+(\S+)\s+(0x[0-9a-f]+)\s+(.+)')
symbols=set()
for label,root in roots:
 with gzip.open(root/'history.txt.gz','rt',errors='replace') as f:
  for line in f:
   m=frame.match(line)
   if m:symbols.add(m[4].rsplit(' + ',1)[0].strip())
names=sorted(symbols);command=['xcrun','llvm-cxxfilt']
r=subprocess.run(command,input='\n'.join(names)+'\n',text=True,capture_output=True,check=True)
demangled=r.stdout.splitlines();assert len(demangled)==len(names)
lookup=dict(zip(names,demangled))
summary={'purpose':__doc__,'demangler_command':command,'demangled_symbols':len(lookup),'selection':'One attribution per malloc block to the first demangled symbol starting with weavepy_ or <weavepy_. Generic alloc/hashbrown wrappers are skipped even if their mangled names mention WeavePy. All non-malloc entries remain separate.','limits':'Instrumented reported live bytes, not requested sizes, churn, resident memory, or Python object counts. Cold cache creation and warm startup are different conditions.','variants':{}}
for label,root in roots:
 cap=json.loads((root/'report.json').read_text());assert cap['returncode']==0
 groups={};nonmalloc=[];total=collections.Counter();current=None;digest=hashlib.sha256()
 def finish(row):
  if row is None:return
  if not row['frames'] or 'libsystem_malloc' not in row['frames'][0]['image']:
   nonmalloc.append(row);return
  total['blocks']+=row['blocks'];total['reported_bytes']+=row['reported_bytes']
  first=next((f for f in row['frames'] if f['demangled'].startswith(('weavepy_','<weavepy_'))),row['frames'][0])
  key=first['demangled']+' + '+first['offset']
  g=groups.setdefault(key,{'site':key,'blocks':0,'reported_bytes':0,'stack_groups':0,'mangled_symbol':first['symbol'],'example_frames':row['frames'][:10]})
  for k in ['blocks','reported_bytes']:g[k]+=row[k]
  g['stack_groups']+=1
 with gzip.open(root/'history.txt.gz','rb') as stream:
  for raw in stream:
   digest.update(raw);line=raw.decode(errors='replace');m=header.match(line)
   if m:finish(current);current={'blocks':int(m[1]),'reported_bytes':int(m[2]),'frames':[]};continue
   m=frame.match(line)
   if m and current is not None:
    pieces=m[4].rsplit(' + ',1);symbol=pieces[0].strip();offset=pieces[1].strip() if len(pieces)==2 else '0'
    current['frames'].append({'index':int(m[1]),'image':m[2],'address':m[3],'symbol':symbol,'offset':offset,'demangled':lookup[symbol]})
 finish(current);assert digest.hexdigest()==cap['history_sha256']
 ordered=sorted(groups.values(),key=lambda g:g['reported_bytes'],reverse=True)
 summary['variants'][label]={'capture_report_sha256':hashlib.sha256((root/'report.json').read_bytes()).hexdigest(),'malloc_totals':dict(total),'sites':ordered,'non_malloc_entries':nonmalloc}
 print(label,dict(total))
 for g in ordered[:15]:print(g['blocks'],g['reported_bytes'],g['site'])
out.write_text(json.dumps(summary,indent=2)+'\n')
