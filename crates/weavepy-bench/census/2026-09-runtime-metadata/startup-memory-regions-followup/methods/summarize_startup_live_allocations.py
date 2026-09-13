"""Group live allocation stacks once per block; keep virtual reservations separate."""
import collections,gzip,hashlib,json,re
from pathlib import Path
out=Path('target/binder-flags-startup-allocation-summary.json');assert not out.exists()
summary={'purpose':__doc__,'limits':'Reported instrumented live bytes, not malloc request sizes, allocation churn, object counts, or resident memory. Each malloc block is attributed once to its first WeavePy symbol. Non-malloc entries, including reserved stacks, remain separate.','variants':{}}
header=re.compile(r'^(\d+) calls? for (\d+) bytes:')
frame=re.compile(r'^(\d+)\s+(\S+)\s+(0x[0-9a-f]+)\s+(.+)')
for label,folder in [('cold','binder-flags-startup-live-allocations'),('warm','binder-flags-startup-live-allocations-warm')]:
 root=Path('target')/folder;capture=json.loads((root/'report.json').read_text());assert capture['returncode']==0
 groups={};nonmalloc=[];current=None;total=collections.Counter();digest=hashlib.sha256()
 def finish(row):
  if row is None:return
  if not row['frames'] or 'libsystem_malloc' not in row['frames'][0]['image']:
   nonmalloc.append(row);return
  total['blocks']+=row['blocks'];total['reported_bytes']+=row['reported_bytes']
  first=next((f for f in row['frames'] if 'weavepy_' in f['symbol']),row['frames'][0])
  key=first['symbol']
  g=groups.setdefault(key,{'site':key,'blocks':0,'reported_bytes':0,'stack_groups':0,'example_frames':row['frames'][:8]})
  for k in ['blocks','reported_bytes']:g[k]+=row[k]
  g['stack_groups']+=1
 with gzip.open(root/'history.txt.gz','rb') as stream:
  for raw in stream:
   digest.update(raw);line=raw.decode(errors='replace');m=header.match(line)
   if m:
    finish(current);current={'blocks':int(m[1]),'reported_bytes':int(m[2]),'frames':[]};continue
   m=frame.match(line)
   if m and current is not None:
    current['frames'].append({'index':int(m[1]),'image':m[2],'address':m[3],'symbol':m[4].rstrip()})
 finish(current);assert digest.hexdigest()==capture['history_sha256']
 ordered=sorted(groups.values(),key=lambda g:g['reported_bytes'],reverse=True)
 summary['variants'][label]={'capture_report':str(root/'report.json'),'capture_report_sha256':hashlib.sha256((root/'report.json').read_bytes()).hexdigest(),'malloc_totals':dict(total),'sites':ordered,'non_malloc_entries':nonmalloc}
 print(label,'malloc totals',dict(total),'non-malloc groups',len(nonmalloc))
 for g in ordered[:12]:print(g['blocks'],g['reported_bytes'],g['site'])
out.write_text(json.dumps(summary,indent=2)+'\n')
