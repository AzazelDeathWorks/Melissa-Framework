"""Developer-only Testcontainers proof; never exposes Docker to model tools.

Use an explicitly installed private environment and an existing immutable SDK
image. Two disposable PostgreSQL fixtures prove rollback and cleanup ownership.
"""
import argparse
import json
import os
from pathlib import Path
import uuid

# Set before importing the package. This test owns both exact container objects
# and cleans them in finally; a privileged daemon-mounted Ryuk is unnecessary.
os.environ['TESTCONTAINERS_RYUK_DISABLED'] = 'true'
from testcontainers.core.container import DockerContainer
import docker.errors

BOOT = r'''
import json,subprocess,time
from pathlib import Path
import psycopg
root=Path('/work');socket=root/'socket';socket.mkdir()
pg_bin=next(Path('/usr/lib/postgresql').glob('*/bin'))
subprocess.run([str(pg_bin/'initdb'),'-D','/work/pg','-U','fixture','--auth-local=trust','--auth-host=reject','--no-locale','--encoding=UTF8'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
log=open('/work/pg.log','wb')
subprocess.Popen([str(pg_bin/'postgres'),'-D','/work/pg','-k',str(socket),'-p','37642','-c','listen_addresses=','-c','shared_buffers=16MB'],stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True)
for attempt in range(200):
 try:
  connection=psycopg.connect(host=str(socket),port=37642,user='fixture',dbname='postgres',autocommit=True)
  break
 except psycopg.OperationalError:time.sleep(.025)
else:raise RuntimeError('database not ready')
connection.execute('CREATE TABLE records(id INTEGER PRIMARY KEY,value TEXT NOT NULL)')
connection.execute("INSERT INTO records VALUES(1,'synthetic')")
connection.execute('BEGIN')
try:
 connection.execute('ALTER TABLE records ADD COLUMN changed INTEGER')
 connection.execute('INSERT INTO missing_table VALUES(1)')
except psycopg.Error:
 connection.execute('ROLLBACK')
else:raise AssertionError('invalid migration unexpectedly passed')
columns=connection.execute("SELECT column_name FROM information_schema.columns WHERE table_name='records' ORDER BY ordinal_position").fetchall()
assert columns==[('id',),('value',)]
assert connection.execute('SELECT * FROM records').fetchall()==[(1,'synthetic')]
connection.close()
print(json.dumps({'ready':True,'invalid_migration_failed':True,'rollback_preserved_schema_and_data':True}))
'''
READ = r'''
import psycopg,json
with psycopg.connect(host='/work/socket',port=37642,user='fixture',dbname='postgres',autocommit=True) as c:
 assert c.execute('SELECT * FROM records').fetchall()==[(1,'synthetic')]
print(json.dumps({'unrelated_fixture_still_ready':True}))
'''


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--image', required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if not args.image.startswith('sha256:') or len(args.image) != 71 or args.out.exists():
        raise ValueError('immutable image and new evidence file required')
    owned = []
    result = {'status': 'NOT_RUN', 'image': args.image, 'model_calls': 0, 'testcontainers': '4.15.0'}
    client = None
    try:
        for label in ('target', 'unrelated-sentinel'):
            fixture = DockerContainer(args.image, docker_client_kw={'environment': {'DOCKER_HOST': 'npipe:////./pipe/dockerDesktopLinuxEngine'}})
            fixture.with_name('melissa-service-test-' + uuid.uuid4().hex)
            fixture.with_kwargs(entrypoint='/bin/sleep', network_mode='none', read_only=True,
                                cap_drop=['ALL'], security_opt=['no-new-privileges:true'], user='65534:65534',
                                mem_limit=512 * 1024 * 1024, pids_limit=128)
            fixture.with_command(['300'])
            fixture.tmpfs = {'/work': 'rw,exec,nosuid,nodev,size=134217728,uid=65534,gid=65534,mode=0700'}
            client = fixture.get_docker_client().client
            assert client.images.get(args.image).id == args.image
            def no_pull(*unused, **kwargs):
                raise RuntimeError('unreviewed image pull denied')
            client.images.pull = no_pull
            owned.append(fixture)
            fixture.start()
            container = fixture.get_wrapped_container()
            container.reload()
            assert container.attrs['HostConfig']['NetworkMode'] == 'none'
            assert container.attrs['HostConfig']['ReadonlyRootfs']
            code, output = container.exec_run(['/opt/melissa-python/bin/python', '-I', '-c', BOOT], workdir='/work')
            assert code == 0, output.decode(errors='replace')
            result[label] = {'container_id': container.id, 'observed': json.loads(output)}
        target_id = owned[0].get_wrapped_container().id
        owned[0].stop(delete_volume=False)
        owned.pop(0)
        try:
            client.containers.get(target_id)
        except docker.errors.NotFound:
            result['target_removed'] = True
        else:
            raise AssertionError('owned target was not removed')
        code, output = owned[0].get_wrapped_container().exec_run(['/opt/melissa-python/bin/python', '-I', '-c', READ])
        assert code == 0, output.decode(errors='replace')
        result.update(json.loads(output))
        result['status'] = 'PASS'
    except Exception as error:
        result.update(status='FAIL', error_type=type(error).__name__, reason=str(error))
        raise
    finally:
        clean = True
        for fixture in reversed(owned):
            try:
                fixture.stop(delete_volume=False)
            except Exception:
                clean = False
        result['owned_cleanup_completed'] = clean
        args.out.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
        print(json.dumps({'status': result['status'], 'owned_cleanup_completed': clean}))


if __name__ == '__main__':
    main()
