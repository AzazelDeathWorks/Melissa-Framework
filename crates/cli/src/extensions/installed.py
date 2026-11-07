import importlib.metadata, json, os, subprocess, sys
ecosystem, package = sys.argv[1:]
if ecosystem == 'python':
    version = importlib.metadata.version(package)
elif ecosystem == 'node':
    version = subprocess.check_output(['node', '-e', "console.log(require(process.argv[1]+'/package.json').version)", package], text=True).strip()
elif ecosystem == 'cargo':
    metadata = json.loads(subprocess.check_output(['cargo','metadata','--locked','--offline','--format-version','1'], text=True))
    versions = {p['version'] for p in metadata['packages'] if p['name'] == package}
    if len(versions) != 1: raise RuntimeError('Missing or ambiguous Cargo dependency version')
    version = versions.pop()
elif ecosystem == 'runtime':
    if package == 'python': version = '.'.join(map(str,sys.version_info[:3]))
    elif package == 'node': version = subprocess.check_output(['node','-p','process.versions.node'],text=True).strip()
    elif package == 'rust': version = subprocess.check_output(['rustc','--version'],text=True).split()[1]
    else: raise RuntimeError('Unsupported runtime')
else: raise RuntimeError('Unsupported ecosystem')
print(json.dumps({'version':version,'package':package,'ecosystem':ecosystem,'interpreter':sys.executable,'environment_prefix':sys.prefix}))
