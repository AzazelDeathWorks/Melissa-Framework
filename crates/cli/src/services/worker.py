"""Trusted task-owned service supervisor, running inside the admitted worker.

All destinations are the registered loopback ports. No daemon access, host
mounts, external connection string, user shell, or dependency downloads.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import shutil
import subprocess
import sys
import time
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path.cwd().resolve()
PRIVATE = ROOT / '.melissa-extension/services'
P = json.loads((PRIVATE / 'input.json').read_bytes())
SPEC = P['spec']
children = {}
database = None
started = False
closed = False
sequence = 0
origins = {name: 'http://127.0.0.1:' + str(s['port']) for name, s in SPEC['services'].items()}
base_environment = dict(os.environ)


def captured_path(value):
    path = ROOT / value
    if path.is_symlink() or not path.resolve().is_relative_to(ROOT):
        raise ValueError('path is outside the captured project')
    return path


def read_bounded(path, capacity=None):
    capacity = capacity if capacity is not None else max(0, (P['max_bytes'] - 65536) // 2)
    if path.stat().st_size > capacity:
        raise OverflowError('reviewed output capacity exceeded')
    with path.open('rb') as source:
        data = source.read(capacity + 1)
    if len(data) > capacity:
        raise OverflowError('reviewed output capacity exceeded')
    return data


def new_database(engine, label, schema=None, sample=None):
    # Reuse the existing SQL parser, read-only query gate, PG roles and schema
    # fingerprint. Each adapter owns a separate database directory/socket.
    namespace = {'__name__': 'melissa_owned_database_' + label,
                 'MELISSA_ADAPTER_PRIVATE': PRIVATE / 'web_private'}
    code = (PRIVATE / 'web.py').read_bytes()
    exec(compile(code, str(PRIVATE / 'web.py'), 'exec'), namespace)
    namespace['PRIVATE'] = PRIVATE / ('db-' + label)
    namespace['PRIVATE'].mkdir()
    namespace['STATE'] = namespace['PRIVATE'] / 'state'
    namespace['R'] = {'app': None, 'database': engine, 'seed_path': None,
                      'services': ['database'], 'migrations': {},
                      'statements': {name: {'source_path': path} for name, path in SPEC['statements'].items()}}
    namespace['P']['postgres_data_policy'] = None
    try:
        namespace['start']()
        for path in [schema, sample]:
            if path:
                apply_sql(namespace, [path], True)
        return namespace
    except Exception:
        namespace['cleanup']()
        raise


def environment():
    env = dict(base_environment)
    env['MELISSA_SERVICE_ORIGINS'] = json.dumps(origins, separators=(',', ':'))
    if database:
        env.update(MELISSA_FIXTURE_DB=str(database['STATE'] / 'fixture.sqlite'),
                   MELISSA_PG_SOCKET=str(database['STATE'] / 'socket'),
                   MELISSA_PG_PORT='37642', MELISSA_PG_DATABASE='postgres',
                   MELISSA_PG_USER='melissa_fixture')
    return env


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def readiness(name):
    spec = SPEC['services'][name]
    child = children[name]['process']
    if child.poll() is not None:
        return False
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(origins[name] + spec['ready']['path'], timeout=0.5) as response:
            return response.status == spec['ready']['status']
    except Exception:
        return False


def resolve_command(command):
    executable = command['executable']
    selected = P['environment']['executables']
    if '/' not in executable:
        executable = selected.get({'python3': 'python', 'rust': 'rustc'}.get(executable, executable), executable)
    argv = [executable, *command['argv']]
    if P['environment']['use_mise']:
        selectors = [k + '@' + v for k, v in P['environment']['versions'].items() if k in ('python', 'node', 'rust')]
        argv = [selected.get('mise', 'mise'), 'exec', *selectors, '--', *argv]
    return argv


def start():
    global database, started
    if started:
        raise ValueError('service bundle already started')
    (PRIVATE / 'logs').mkdir()
    if SPEC.get('database'):
        spec = SPEC['database']
        database = new_database(spec['engine'], 'service', spec.get('schema'), spec.get('sample'))
    pending = dict(SPEC['services'])
    ready = {'database'} if database else set()
    try:
        while pending:
            progressed = False
            for name, spec in list(pending.items()):
                if not all(d in ready for d in spec['depends_on']):
                    continue
                command = spec['command']
                stdout = PRIVATE / 'logs' / (name + '.stdout')
                stderr = PRIVATE / 'logs' / (name + '.stderr')
                with stdout.open('wb') as out, stderr.open('wb') as err:
                    child = subprocess.Popen(resolve_command(command), cwd=captured_path(command['cwd']),
                                             env=dict(environment(), PORT=str(spec['port'])),
                                             stdin=subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True)
                children[name] = {'process': child, 'stdout': stdout, 'stderr': stderr}
                deadline = time.monotonic() + spec['ready']['timeout_ms'] / 1000
                while not readiness(name):
                    if child.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('service readiness failed: ' + name)
                    time.sleep(0.025)
                ready.add(name)
                del pending[name]
                progressed = True
            if not progressed:
                raise ValueError('service dependency cycle')
        started = True
        return status()
    except Exception as error:
        logs = {}
        for name, item in children.items():
            logs[name] = {stream + '_hex': read_bounded(item[stream]).hex() for stream in ('stdout', 'stderr')}
        cleanup()
        raise StartupFailure({'reason': type(error).__name__, 'service_logs': logs}) from error


class StartupFailure(Exception):
    pass


def status():
    values = {name: {'ready': readiness(name), 'exit_code': item['process'].poll(),
                     'port': SPEC['services'][name]['port'], 'origin': origins[name],
                     'visibility': 'worker_loopback_only'} for name, item in children.items()}
    db = None
    if database:
        db = database['command']({'command': 'profile', 'connection': 'health'})
        db['engine'] = SPEC['database']['engine']
        db['ready'] = True
    return {'services': values, 'database': db, 'ready': all(v['ready'] for v in values.values()),
            'host_port_published': False}


def log_sources():
    sources = {name: {stream: item[stream] for stream in ('stdout', 'stderr')} for name, item in children.items()}
    if database and SPEC['database']['engine'] == 'postgres':
        sources['database'] = {'stderr': database['STATE'] / 'pg.log'}
    return sources


def log_page(name, stream, offset, maximum):
    sources = log_sources()
    if name not in sources or stream not in sources[name] or not 1 <= maximum <= 32768:
        raise ValueError('unregistered service log page')
    path = sources[name][stream]
    size = path.stat().st_size
    if offset < 0 or offset > size:
        raise ValueError('log cursor beyond observed size')
    with path.open('rb') as source:
        source.seek(offset)
        data = source.read(maximum)
    return {'service': name, 'stream': stream, 'offset': offset, 'next_offset': offset + len(data),
            'observed_size': size, 'data_hex': data.hex(), 'complete_at_observation': offset + len(data) == size}


def api(name, request):
    # Existing API adapter refuses redirects and proxies, retains cookies and
    # records uncertain writes. Only the registered local origin is substituted.
    global api_adapter
    if name not in origins:
        raise ValueError('unknown service destination')
    if 'api_adapter' not in globals():
        namespace = {'__name__': 'melissa_owned_api', 'MELISSA_ADAPTER_PRIVATE': PRIVATE / 'web_private'}
        exec(compile((PRIVATE / 'web.py').read_bytes(), str(PRIVATE / 'web.py'), 'exec'), namespace)
        api_adapter = namespace
    api_adapter['ORIGIN'] = origins[name]
    return api_adapter['api'](request)


def sql_operation(operation):
    if not database:
        raise ValueError('no registered database')
    return database['command'](operation)


def apply_sql(adapter, paths, transactional):
    connection = adapter['connection']('rehearsal-write')
    forbidden = {'BEGIN', 'COMMIT', 'END', 'ROLLBACK', 'SAVEPOINT', 'RELEASE', 'VACUUM',
                 'ATTACH', 'DETACH', 'PRAGMA', 'ABORT', 'START', 'PREPARE', 'COPY', 'SET'}
    statements = []
    for path in paths:
        for sql in adapter['statements'](captured_path(path).read_text()):
            if adapter['sql_keyword'](sql) in forbidden:
                raise ValueError('migration control statement denied')
            statements.append(sql)
    if not statements:
        raise ValueError('empty migration script')
    applied = 0
    if transactional:
        connection.execute('BEGIN IMMEDIATE' if adapter['R']['database'] == 'sqlite' else 'BEGIN')
    try:
        for sql in statements:
            connection.execute(sql)
            applied += 1
        if transactional:
            connection.execute('COMMIT')
        return {'applied': applied, 'transactional': transactional}
    except Exception as error:
        rolled_back = False
        if transactional:
            connection.execute('ROLLBACK')
            rolled_back = True
        raise MigrationFailure({'applied_before_error': applied, 'rolled_back': rolled_back,
                                'transactional': transactional, 'error_type': type(error).__name__}) from error


class MigrationFailure(Exception):
    pass


def assertions(adapter, expected):
    results = []
    for index, check in enumerate(expected):
        result = adapter['query']({'connection': 'invariants', 'sql': check['sql'], 'parameters': []})
        if result['truncated']:
            raise OverflowError('invariant query inventory is incomplete')
        results.append({'index': index, 'passed': result['rows'] == check['rows'], 'observed': result['rows']})
    return {'passed': all(r['passed'] for r in results), 'checks': results}


def logical_state(adapter):
    c = adapter['connection']('fingerprint')
    if adapter['R']['database'] == 'sqlite':
        names = c.execute("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetchall()
    else:
        names = c.execute("SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' ORDER BY table_name").fetchall()
    values = []
    size = 0
    for (name,) in names:
        rows = c.execute('SELECT * FROM "' + name.replace('"', '""') + '"').fetchall()
        encoded = sorted(json.dumps(list(row), sort_keys=True, default=str, separators=(',', ':')) for row in rows)
        size += sum(map(len, encoded))
        if size > P['max_bytes']:
            raise OverflowError('logical state exceeds reviewed evidence capacity')
        values.append([name, encoded])
    return {'schema': adapter['schema'](c), 'data': 'sha256:' + hashlib.sha256(json.dumps(values, separators=(',', ':')).encode()).hexdigest()}


def dispose_rehearsal(adapter, result, label):
    adapter['cleanup']()
    log = adapter['STATE'] / 'pg.log'
    if log.exists():
        result.setdefault('database_logs', {})[label] = {'pg_log_hex': read_bounded(log).hex()}
    # Physical DB files are disposable runtime state. Logical schema/data proof,
    # failed attempts and raw PG logs remain in the operation's retained detail.
    data = adapter['STATE'] / 'pg'
    if data.exists():
        if data.is_symlink() or not data.resolve().is_relative_to(PRIVATE) or data.parent != adapter['STATE']:
            raise ValueError('rehearsal data disposal ownership mismatch')
        shutil.rmtree(data)


def migration_rehearsal(name):
    spec = SPEC['rehearsals'][name]
    result = {'outcome': 'INCONCLUSIVE', 'name': name, 'engine': spec['engine'],
              'provenance': spec['provenance'], 'production_connection': False,
              'rollback': {'supported': spec.get('downgrade') is not None, 'status': 'NOT_RUN' if spec.get('downgrade') is not None else 'NOT_SUPPORTED'}}
    fresh = upgraded = None
    try:
        fresh = new_database(spec['engine'], 'fresh-' + str(sequence))
        result['fresh_apply'] = apply_sql(fresh, spec['fresh'], spec['transactional'])
        apply_sql(fresh, [spec.get('fresh_sample') or spec['sample']], True)
        result['fresh_invariants'] = assertions(fresh, spec['after'])
        fresh_state = logical_state(fresh)
        upgraded = new_database(spec['engine'], 'upgrade-' + str(sequence), spec['baseline'], spec['sample'])
        result['before_invariants'] = assertions(upgraded, spec['before'])
        before = logical_state(upgraded)
        result['before_state'] = before
        try:
            result['upgrade_apply'] = apply_sql(upgraded, spec['upgrade'], spec['transactional'])
        except MigrationFailure as error:
            result['upgrade_failure'] = error.args[0]
            result['failure_state'] = logical_state(upgraded)
            result['failure_preserved_original'] = result['failure_state'] == before
            result['partial_effects'] = not result['failure_preserved_original']
            result['outcome'] = 'FAIL'
            return result
        after = logical_state(upgraded)
        result['after_state'] = after
        result['after_invariants'] = assertions(upgraded, spec['after'])
        result['fresh_upgrade_schema_equal'] = fresh_state['schema'] == after['schema']
        if spec.get('downgrade') is not None:
            result['rollback']['apply'] = apply_sql(upgraded, spec['downgrade'], spec['transactional'])
            result['rollback']['state_restored'] = logical_state(upgraded) == before
            result['rollback']['invariants'] = assertions(upgraded, spec['before'])
            result['rollback']['status'] = 'PASS' if result['rollback']['state_restored'] and result['rollback']['invariants']['passed'] else 'FAIL'
        passed = all(result[key]['passed'] for key in ('fresh_invariants', 'before_invariants', 'after_invariants'))
        passed = passed and result['fresh_upgrade_schema_equal'] and result['rollback']['status'] != 'FAIL'
        result['outcome'] = 'PASS' if passed else 'FAIL'
        return result
    except MigrationFailure as error:
        result.update(outcome='FAIL', migration_failure=error.args[0])
        return result
    finally:
        if fresh:
            dispose_rehearsal(fresh, result, 'fresh')
        if upgraded:
            dispose_rehearsal(upgraded, result, 'upgrade')


def contract_check(name):
    check = SPEC['api_checks'][name]
    folder = PRIVATE / ('contract-' + str(sequence))
    folder.mkdir()
    (folder / 'config.toml').write_text('')
    junit = folder / 'results.xml'
    argv = ['/opt/melissa-contracts/bin/schemathesis', '--config-file', str(folder / 'config.toml'), 'run',
            str(captured_path(check['schema'])), '--url', origins[check['service']],
            '--seed', str(check['seed']), '--max-examples', str(check['examples']), '--mode', 'all',
            '--phases', ','.join(check['phases']), '--report', 'junit', '--report-junit-path', str(junit),
            '--output-sanitize', 'true', '--output-truncate', 'false']
    with (folder / 'stdout').open('wb') as out, (folder / 'stderr').open('wb') as err:
        completed = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=out, stderr=err, env=environment())
    stdout = read_bounded(folder / 'stdout')
    stderr = read_bounded(folder / 'stderr')
    if not junit.exists():
        return {'outcome': 'BLOCKED', 'reason': 'contract_runner_report_missing', 'exit_code': completed.returncode,
                'seed': check['seed'], 'phases': check['phases'], 'stdout_hex': stdout.hex(), 'stderr_hex': stderr.hex()}
    raw = read_bounded(junit)
    cases = list(ET.fromstring(raw).iter('testcase'))
    failed = sum(c.find('failure') is not None or c.find('error') is not None for c in cases)
    skipped = sum(c.find('skipped') is not None for c in cases)
    outcome = 'FAIL' if completed.returncode or failed else 'NOT_RUN' if skipped else 'PASS' if cases else 'INCONCLUSIVE'
    return {'outcome': outcome, 'seed': check['seed'], 'phases': check['phases'], 'mode': 'all',
            'exit_code': completed.returncode, 'operation_counts': {'total': len(cases), 'failed': failed, 'skipped': skipped},
            'test_data_may_have_changed': True, 'stdout_hex': stdout.hex(), 'stderr_hex': stderr.hex(), 'junit_hex': raw.hex()}


def business_check(name):
    if name not in SPEC['business_checks']:
        raise ValueError('business check not registered')
    verification = ROOT / '.melissa-extension/verification'
    verification.mkdir(exist_ok=True)
    payload = {'command': P['checks'][name], 'check': P['check_options'][name],
               'environment': P['environment'], 'max_bytes': P['max_bytes'] // 3}
    (verification / 'input.json').write_text(json.dumps(payload))
    attempts = []
    for _ in range(payload['check']['repetitions']):
        completed = subprocess.run(['/usr/bin/python3', '-I', str(verification / 'worker.py')],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment())
        if completed.returncode or len(completed.stdout) > P['max_bytes']:
            raise RuntimeError('business check collector failed')
        attempts.append(json.loads(completed.stdout))
        if sum(len(json.dumps(a)) for a in attempts) > P['max_bytes'] // 2:
            raise OverflowError('complete repetitions exceed reviewed evidence capacity')
    statuses = [a['outcome'] for a in attempts]
    outcome = next((s for s in ('INCONCLUSIVE', 'BLOCKED', 'NOT_RUN', 'FLAKY') if s in statuses), None)
    if outcome is None:
        outcome = 'FLAKY' if 'PASS' in statuses and 'FAIL' in statuses else 'FAIL' if 'FAIL' in statuses else 'PASS'
    result = dict(attempts[0])
    result.update(outcome=outcome, requested_repetitions=payload['check']['repetitions'], attempts=attempts)
    return result



def cleanup():
    global closed
    for item in reversed(list(children.values())):
        process = item['process']
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=3)
            except (subprocess.TimeoutExpired, ProcessLookupError):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=3)
    if database:
        database['cleanup']()
    closed = True


def operation(value):
    kind = value['operation']
    if kind == 'start':
        return start()
    if kind == 'close':
        cleanup()
        logs = {}
        remaining = max(0, (P['max_bytes'] - 65536) // 2)
        try:
            for name, item in log_sources().items():
                logs[name] = {}
                for stream, path in item.items():
                    data = read_bounded(path, remaining)
                    remaining -= len(data)
                    logs[name][stream + '_hex'] = data.hex()
            return {'closed': True, 'native_stop_proof_required': True, 'log_capture': 'PASS', 'service_logs': logs}
        except OverflowError:
            sizes = {name: {stream: path.stat().st_size for stream, path in item.items()} for name, item in log_sources().items()}
            return {'outcome': 'BLOCKED', 'closed': True, 'native_stop_proof_required': True,
                    'log_capture': 'BLOCKED', 'reason': 'complete_logs_exceed_reviewed_output_capacity', 'log_sizes': sizes}

    if not started or closed:
        raise ValueError('service bundle is not active')
    if kind == 'status':
        return status()
    if kind == 'log':
        return log_page(value['service'], value['stream'], value['offset'], value['max_bytes'])
    if kind == 'api':
        return api(value['service'], value['request'])
    if kind == 'database':
        return sql_operation(value['command'])
    if kind == 'contract':
        return contract_check(value['name'])
    if kind == 'migration':
        return migration_rehearsal(value['name'])
    if kind == 'business':
        return business_check(value['name'])
    raise ValueError('unregistered service operation')


class SourceChanged(Exception):
    pass


try:
    for line in sys.stdin:
        request = json.loads(line)
        if request['seq'] != sequence + 1:
            raise ValueError('non-sequential service operation; no replay')
        sequence = request['seq']
        try:
            historical = request['operation']['operation'] in ('status', 'log', 'close')
            if not historical and not verify_source_manifest(ROOT, P):
                raise SourceChanged('worker source no longer matches captured input')
            body = operation(request['operation'])
            outcome = body.get('outcome', 'FAIL' if body.get('ready') is False or body.get('effect') == 'read_failed' else 'PASS')
            uncertain = body.get('effect') == 'outcome_unknown' or body.get('database_effect_unknown', False)
            if uncertain:
                outcome = 'INCONCLUSIVE'
            source_current = verify_source_manifest(ROOT, P)
            if not historical and not source_current:
                outcome, uncertain = 'INCONCLUSIVE', True
            reply = {'outcome': outcome, 'body': body, 'requires_reconciliation': uncertain, 'worker_source_current': source_current}
        except StartupFailure as error:
            reply = {'outcome': 'FAIL', 'reason': 'service_readiness_failed', 'body': error.args[0], 'requires_reconciliation': False}
        except SourceChanged:
            reply = {'outcome': 'BLOCKED', 'reason': 'worker_source_changed_before_operation', 'requires_reconciliation': False}
        except FileNotFoundError:
            reply = {'outcome': 'BLOCKED', 'reason': 'registered_tool_or_file_missing', 'requires_reconciliation': False}
        except OverflowError:
            reply = {'outcome': 'BLOCKED', 'reason': 'reviewed_evidence_capacity_exceeded', 'requires_reconciliation': False}
        except Exception as error:
            uncertain = request['operation']['operation'] == 'database' and database is not None and database['uncertain_sql_error'](request['operation']['command'], error)
            reply = {'outcome': 'INCONCLUSIVE' if uncertain else 'FAIL', 'reason': type(error).__name__, 'requires_reconciliation': uncertain}
        reply.update(event='service_reply', seq=sequence, source_digest=P['source_digest'], task_accepted=False)
        raw = json.dumps(reply, separators=(',', ':'), ensure_ascii=True)
        if len(raw.encode()) > P['max_bytes']:
            raise OverflowError('complete response cannot fit reviewed output capacity')
        print(raw, flush=True)
        if closed:
            break
finally:
    cleanup()
