"""Fixed collector inside an admitted project snapshot; no host/env export."""
import json, os, re, shutil, subprocess, sys
from pathlib import Path
import xml.etree.ElementTree as ET
ROOT=Path.cwd().resolve()
PRIVATE=ROOT/'.melissa-extension/verification'
P=json.loads((PRIVATE/'input.json').read_bytes())
raw_out=b''; raw_err=b''; exit_code=None
raw_budget=max(0,(P['max_bytes']-65536)//2)
record={'schema_version':'melissa.project-check/v1','outcome':'INCONCLUSIVE','kind':P['check']['kind'],'test_counts':None,'reason':None,'project_result_untrusted':True}
def source_path(value):
    path=ROOT/value
    if path.is_symlink() or not path.resolve().is_relative_to(ROOT): raise ValueError('report path outside snapshot')
    return path
def bounded_read(path):
    global raw_budget
    size=path.stat().st_size
    if size>raw_budget: raise OverflowError('combined logs/report exceed reviewed output capacity')
    data=path.read_bytes()
    if len(data)>raw_budget: raise OverflowError('output changed while reading')
    raw_budget-=len(data)
    return data
def junit(data):
    root=ET.fromstring(data)
    cases=list(root.iter('testcase'))
    counts={'passed':0,'failed':0,'skipped':0,'flaky':0,'total':len(cases)}
    for c in cases:
        if c.find('skipped') is not None: counts['skipped']+=1
        elif c.find('failure') is not None or c.find('error') is not None: counts['failed']+=1
        else: counts['passed']+=1
        if any('rerun' in x.tag.lower() or 'flaky' in x.tag.lower() for x in c): counts['flaky']+=1
    return counts
try:
    command=P['command']; cwd=source_path(command['cwd'])
    args=[command['executable'],*command['argv']]
    project_environment=P['environment']
    if '/' not in args[0]:
        name={'python3':'python','rust':'rustc'}.get(args[0],args[0])
        args[0]=project_environment['executables'].get(name) or shutil.which(name)
        if not args[0]: raise FileNotFoundError('selected executable unavailable')
    tool=Path(args[0]).name
    if tool=='cargo' and len(args)>1 and args[1] in ('nextest','llvm-cov','mutants'):
        if not shutil.which('cargo-'+args[1]): raise FileNotFoundError('required quality tool not installed in reviewed image')
    environment=dict(os.environ,CARGO_NET_OFFLINE='true',CARGO_LLVM_COV_SETUP='no')
    if project_environment['use_mise']:
        mise=project_environment['executables'].get('mise') or shutil.which('mise')
        if not mise: raise FileNotFoundError('selected mise unavailable')
        selectors=[k+'@'+v for k,v in project_environment['versions'].items() if k in ('python','node','rust')]
        args=[mise,'exec',*selectors,'--',*args]
    report=P['check'].get('report')
    report_path=source_path(report['path']) if report else None
    report_before=report_path.stat() if report_path and report_path.exists() else None
    # Files, not unbounded PIPE buffers. The enclosing tmpfs/output/deadline
    # limits and process-tree cancellation remain the native authority.
    out_path=PRIVATE/'command.stdout';err_path=PRIVATE/'command.stderr'
    with out_path.open('wb') as out,err_path.open('wb') as err:
        process=subprocess.Popen(args,cwd=cwd,env=environment,stdin=subprocess.DEVNULL,stdout=out,stderr=err)
        exit_code=process.wait()
    raw_out=bounded_read(out_path);raw_err=bounded_read(err_path)
    data=None
    if report:
        report_path=source_path(report['path'])
        after=report_path.stat()
        if report_before and (report_before.st_ino,report_before.st_size,report_before.st_mtime_ns,report_before.st_ctime_ns)==(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns):
            raise RuntimeError('report was not refreshed by this command')
        data=bounded_read(source_path(report['path']))
        record['report_hex']=data.hex()
    kind=P['check']['kind']
    counts=None
    if report and report['format']=='junit': counts=junit(data)
    elif kind in ('test','doctest','regression'):
        text=raw_out.decode('utf-8','replace')
        summaries=re.findall(r'^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;',text,re.M)
        if summaries:
            passed=sum(int(x[0]) for x in summaries);failed=sum(int(x[1]) for x in summaries);skipped=sum(int(x[2]) for x in summaries)
            counts={'passed':passed,'failed':failed,'skipped':skipped,'flaky':0,'total':passed+failed+skipped}
        else:
            values=dict(re.findall(r'^# (tests|pass|fail|cancelled|skipped) (\d+)\s*$',text,re.M))
            if 'tests' in values:
                counts={'total':int(values['tests']),'passed':int(values.get('pass',0)),'failed':int(values.get('fail',0))+int(values.get('cancelled',0)),'skipped':int(values.get('skipped',0)),'flaky':0}
        if counts is None:
            text=re.sub(r'\x1b\[[0-9;]*m','',text+'\n'+raw_err.decode('utf-8','replace'))
            summaries=re.findall(r'^(?:=+\s*)?((?:\d+ (?:passed|failed|errors?|skipped|xfailed|xpassed|deselected|rerun|warnings?)(?:, )?)+) in [\d.]+s(?:.*)$',text,re.M)
            if summaries:
                values={k:int(n) for n,k in re.findall(r'(\d+) (passed|failed|errors?|skipped|xfailed|xpassed|deselected|rerun)',summaries[-1])}
                passed=values.get('passed',0);failed=sum(values.get(k,0) for k in ('failed','error','errors','xpassed'));skipped=values.get('skipped',0)+values.get('xfailed',0)
                counts={'total':passed+failed+skipped,'passed':passed,'failed':failed,'skipped':skipped,'flaky':values.get('rerun',0),'deselected':values.get('deselected',0)}
            else:
                ran=re.findall(r'^Ran (\d+) tests? in [\d.]+s\s*$',text,re.M)
                final=re.search(r'^(OK|FAILED)(?: \(([^\n]*)\))?\s*$',text,re.M)
                if ran and final:
                    values={k:int(n) for k,n in re.findall(r'(failures|errors|skipped)=(\d+)',final[2] or '')}
                    total=int(ran[-1]);failed=values.get('failures',0)+values.get('errors',0);skipped=values.get('skipped',0)
                    counts={'total':total,'passed':total-failed-skipped,'failed':failed,'skipped':skipped,'flaky':0}
    if kind=='nextest':
        summary=raw_err.decode('utf-8','replace')
        skipped=re.findall(r'(\d+) skipped',summary)
        flaky=re.findall(r'(\d+) flaky',summary)
        if counts is not None and flaky:
            counts['flaky']=max(counts['flaky'],int(flaky[-1]))
        if counts is not None and skipped and int(skipped[-1])>counts['skipped']:
            counts['skipped']=int(skipped[-1])
            counts['total']=counts['passed']+counts['failed']+counts['skipped']
            record['reason']='runner_skips_not_all_present_in_junit'
    if kind in ('test','doctest','nextest','regression'):
        if counts is None: record.update(outcome='FAIL' if exit_code else 'INCONCLUSIVE',reason='machine_readable_test_counts_unavailable')
        elif counts['flaky']: record.update(outcome='FLAKY',reason='runner_retry_recovered_failure')
        elif not counts['total'] or counts['passed']+counts['failed']+counts['skipped']!=counts['total']: record.update(outcome='INCONCLUSIVE',reason='empty_or_inconsistent_test_inventory')
        elif exit_code or counts['failed']: record['outcome']='FAIL'
        elif counts['skipped']: record.update(outcome='NOT_RUN',reason='selected_tests_skipped')
        else: record['outcome']='PASS'
        record['test_counts']=counts
    elif kind=='coverage':
        if not report or report['format']!='llvm_json': record.update(outcome='INCONCLUSIVE',reason='coverage_report_unavailable')
        else:
            value=json.loads(data); totals=value['data'][0]['totals']; record['coverage']=totals
            record['outcome']='PASS' if exit_code==0 and totals['lines']['count']>0 else 'FAIL'
    elif kind=='mutation':
        if not report or report['format']!='mutants_json': record.update(outcome='INCONCLUSIVE',reason='mutation_report_unavailable')
        else:
            value=json.loads(data)
            record['mutation_counts']={k:value.get(k) for k in ('total_mutants','caught','missed','timeout','unviable','success')}
            # Baseline and every result remain explicit. Unviable/timeout never
            # count as a caught mutant and do not improve a coverage percentage.
            if value.get('missed',0) or value.get('timeout',0) or exit_code: record['outcome']='FAIL'
            elif value.get('caught',0)>0: record['outcome']='PASS'
            else: record.update(outcome='INCONCLUSIVE',reason='no_viable_caught_mutants')
    else: record['outcome']='PASS' if exit_code==0 else 'FAIL'
except FileNotFoundError:
    record.update(outcome='BLOCKED',reason='required_tool_or_report_missing')
except OverflowError:
    record.update(outcome='BLOCKED',reason='reviewed_output_capacity_exceeded')
except Exception:
    record.update(outcome='INCONCLUSIVE',reason='collector_or_report_validation_failed')
record.update(command_exit_code=exit_code,raw_stdout_hex=raw_out.hex(),raw_stderr_hex=raw_err.hex())
sys.stdout.write(json.dumps(record,separators=(',',':'),ensure_ascii=True)+'\n')
