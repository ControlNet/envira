# Deep review, round 2 (2026-11-13)

**Status (2026-11-28):** every finding below was fixed in `573f824`, and the gaps the fix verification found (F1-F7) in `dca939d`; both shipped in 0.1.6. See `deep-review-fix-verification-2026-11-13.md`.

Reviewed commit: `491f7e3`, envira 0.1.5. This continues
`deep-review-2026-11-13.md`; findings R1-R10 are not repeated. No production source,
catalog, project rules, real installation, or real user configuration was changed.

## Scope and evidence

This pass examined privilege transitions, backup recovery, interrupted updates and retry,
exported execution, overlapping TUI refreshes, bootstrap installation, and lock/self-update
paths. The eight findings below have isolated reproductions using the real debug binary or
the real bootstrap script. No additional lock or Rust self-update defect is claimed here.

The previous pass's 269 Rust tests, 11 Python tests, Clippy, formatting and catalog checks
passed. They were not rerun against unchanged production code in this pass. Passing those
checks did not cover the failure paths reproduced below. Native macOS, the complete distro
matrix, actual vendor downloads, and disk-full/power-loss injection remain untested.

All reproduction inputs are explicitly **synthetic test fixtures**. The Go case uses the
actual catalog recipe with a synthetic existing Go executable and a synthetic curl response
whose transport succeeds but whose archive body is invalid. The backup case deliberately
corrupts only temporary metadata. Other local cases use fresh temporary HOME/state directories.
The bootstrap case serves the current real build and its SHA-256 over loopback HTTP. The sudo
case runs only inside a disposable container with networking disabled; its account named
`user` is a container fixture, not a real person's identity. No credentials are required.

## Confirmed findings

### R11 — P1: Go update removes the old toolchain before validating extraction

Location: `catalog/toolchains.toml:41`.

After curl succeeds, the recipe deletes `~/.go`, recreates it, and only then extracts the
archive. A successful HTTP transfer does not establish archive validity or successful
extraction. In the isolated reproduction, tar rejected the synthetic invalid body and the
previous `~/.go/bin/go` no longer existed. The update failed, but a previously usable
installation had already been destroyed. Extraction errors or an interrupted extraction
have the same unsafe ordering; those additional failure modes were not injected.

Fix direction: download and extract into a separate staging directory, validate the resulting
executables, then switch installations with a preserved old directory and a rollback path.
Never delete the working installation before the replacement is ready.

### R12 — P2: unreadable or invalid backup metadata can overwrite the original backup

Locations: `src/fs.rs:84`, `src/fs.rs:89`, `src/fs.rs:131`, `src/fs.rs:151`.

Metadata loading converts read and parse errors to `None`, which `ensure` treats as a missing
backup. It then copies the currently modified file over the existing `orig`. Reproduction:
write `original user data`, apply `first change`, corrupt the temporary `meta.toml`, apply
`second change`, restore. All editing/restoring commands exit 0, but restore yields
`first change`; the original data is gone. The corrupt metadata is the explicit trigger,
not a claim that envira normally writes malformed TOML.

Fix direction: distinguish missing metadata from unreadable/corrupt metadata; preserve the
existing backup and stop on the latter. Existing `orig` must not be overwritten merely
because metadata could not be parsed. Recovery should be explicit and inspectable.

### R13 — P2: exported scripts omit the executor's working-directory isolation

Locations: `src/plan.rs:1065`, `src/exec.rs:658`, `src/runner.rs:765`.

Native execution creates a temporary directory per unit and sets both cwd and `ENVIRA_TMP`.
`as_script` emits commands directly after `set -eo pipefail`, with no equivalent directory
setup. The same synthetic install action left a caller's `payload` file unchanged through
`envira install`, but overwrote it when exported using `plan --script` and run with bash.
Both executions exited 0. Production recipes also use relative download/extraction names,
including `go.tar.gz`; the behavioral difference is not confined to the fixture.

Fix direction: make exported execution preserve per-unit cwd, necessary environment,
scope/user context and cleanup. Add a behavioral comparison between native and exported
execution, including a caller directory containing colliding filenames.

### R14 — P2: changing between normal and sudo entrypoints loses system-config backups

Locations: `src/fs.rs:14`, `src/runner.rs:167`, `src/template.rs:120`.

Normal execution exports `ENVIRA_STATE_DIR` to its shell, but a system action invoking
`sudo envira fs ...` passes through sudo's environment reset. Its backup falls back to the
root account's state directory. With `sudo envira`, the template omits that inner sudo and
the runner's explicit user-state directory reaches the helper. Consequently, applying a
system config as a normal user and reverting it through `sudo envira` searches a different
backup namespace. In the offline container, apply exited 0, revert exited 1 with
`no envira backup for /etc/envira-review.conf`, and the modified file remained. A backup
existed under the root account, but no matching backup existed in the user's state directory.

Fix direction: define a stable, protected backup namespace for system resources and pass
the chosen context explicitly across privilege boundaries. Avoid depending on whether sudo
happens to preserve environment variables, or broadly preserving arbitrary environment.

### R15 — P2: retry cannot recover an update that removed the detected executable

Locations: `src/commands.rs:809`, `src/commands.rs:814`, `src/status.rs:139`.

Retry identifies unfinished update units, then sends them through normal update eligibility
using a fresh snapshot. A missing executable is rejected as `not installed`. After R11's
failed update, `retry -y` exited 1 with `Nothing to do` and
`skip go: not installed (use envira install)`. It did not retry the failed recipe.
This is a separate recovery-policy defect: any interrupted replacement that leaves the
binary absent can hit this gate, even after fixing Go's particular recipe.

Fix direction: use the failed run's action, route and progress to plan recovery, including
an explicit repair/reinstall path when the executable disappears. Normal update eligibility
and failed-run recovery cannot use the same presence gate unconditionally.

### R16 — P2: an older TUI refresh overwrites newer results and their cache

Locations: `src/tui/mod.rs:184`, `src/tui/mod.rs:212`, `src/tui/mod.rs:309`.

Each refresh launches an independent task. Snapshot/latest events carry no generation, and
each task saves its private cache. In the real TUI, the initial synthetic latest query waited
five seconds before returning `2.0.0`. Pressing `R` launched a second query that immediately
returned `3.0.0`. The cache first held `3.0.0`, then regressed to `2.0.0` after the earlier
request completed. The older latest event is also accepted unconditionally by the UI.

Fix direction: assign a generation to each refresh and reject obsolete results before both
UI application and cache persistence. Cancellation can reduce wasted work but does not alone
replace the stale-result check. Apply the same ownership rule to snapshot responses.

### R17 — P2: line checks accept an overridden shell assignment

Locations: `src/fs.rs:616`, `src/fs.rs:618`, `catalog/shell.toml:49`.

The line helper selects only the first regex match. If that line is already correct, both
check and edit immediately succeed even if a later matching line overrides it. A temporary
shell file containing `export DISABLE_UPDATE_PROMPT=true` followed by the same assignment
with `false` passed the actual catalog pattern's check and edit with exit 0. Sourcing it
still produced `false`. Thus presence/idempotency checks can claim that a setting is applied
when the shell's effective value disagrees.

Fix direction: define duplicate-match semantics, detect conflicting matches, and either
normalize them with complete restoration data or report the conflict. A first-match-only
check is insufficient for last-assignment-wins configuration formats.

### R18 — P2: bootstrap reports success when its destination is a directory

Location: `envira.sh:78`.

Both `mv -f` calls allow directory destinations. If `envira` already names a directory, the
second move puts `.envira.new` inside it. If `.envira.new` already names a directory, the
first move puts the binary inside that directory and the second renames the directory to
`envira`. Both cases were reproduced with the real build, its valid checksum, and a local
HTTP server. With `ENVIRA_NO_LAUNCH=1`, bootstrap exited 0 and printed `installed`, while
the final `envira` path was a directory rather than an executable file.

Fix direction: reject incompatible destination types, use a unique staging file in the
destination filesystem, and use replacement semantics that cannot interpret the target as a
directory. Preserve portability across supported systems; do not assume GNU-only mv flags.

## Design implications and order of repair

1. Protect existing installations and original backups before doing additional work. R11 and
   R12 turn recoverable errors into loss of previously working state.
2. Give execution context a single representation consumed by native execution, exported
   scripts and privileged helpers. Today cwd and backup identity depend on the entrypoint.
3. Treat recovery as its own operation driven by recorded effects, rather than rebuilding an
   ordinary update solely from the current partially broken machine state.
4. Give asynchronous observations an explicit generation before letting them update shared
   cache or UI state.

## Replaying the evidence

Run from the repository root on Linux. Python 3.11+ standard library is sufficient; no Python
packages or environment installation is needed. Build the actual binary first:

```sh
cargo build --locked
```

The first three Python blocks below are independent host-side reproductions. This command
extracts and runs them in order; each creates fresh scratch directories and leaves them for
inspection. Expected signals are described after the command. It does not run the container
fixture on the host.

```sh
python3 - <<'PY'
from pathlib import Path
import re
import subprocess
import sys
import tempfile

note = Path('.omo/knowledges/deep-review-2026-11-13-round2.md').read_text()
out = Path(tempfile.mkdtemp(prefix='envira-review-replay-'))
for index, source in enumerate(re.findall(r'^```python\n(.*?)^```', note, re.M | re.S), 1):
    script = out / f'case-{index}.py'
    script.write_text(source)
    subprocess.run([sys.executable, str(script)], check=True)
PY
```

Expected vulnerable-build signals:

- Main fixture: Go update fails; previous binary exists is `False`; retry skips it as not
  installed; native caller data is preserved and exported caller data becomes `changed`;
  backup restores `first change`; duplicate shell setting evaluates to `false` despite
  successful checks.
- Bootstrap fixture: both cases exit 0 and `FINAL TARGET IS DIRECTORY` is `True`.
- TUI fixture: second request executes; cache version changes from `3.0.0` back to `2.0.0`.

For the privilege case, the existing `envira-it-ubuntu` integration image must be available
with its test account `user` and passwordless sudo. The following extracts the container-only
block and mounts the script and binary read-only. All system-file writes occur inside the
disposable, network-disabled container.

```sh
review_script=$(mktemp /tmp/envira-review-container-XXXXXX.py)
python3 - "$review_script" <<'PY'
from pathlib import Path
import re
import sys
note = Path('.omo/knowledges/deep-review-2026-11-13-round2.md').read_text()
source = re.search(r'^```python-container\n(.*?)^```', note, re.M | re.S).group(1)
Path(sys.argv[1]).write_text(source)
PY
docker run --rm --network none --user root \
  -v "$PWD/target/debug/envira:/opt/envira-review:ro" \
  -v "$review_script:/review.py:ro" \
  envira-it-ubuntu python3 /review.py
```

Expected: normal apply exits 0; sudo-entrypoint revert exits 1; config content remains `new`;
root backup exists is `True` and user system backup exists is `False`.

### Main local failure-path reproduction

```python
"""Review-only synthetic fixtures: no real tools, downloads or credentials are modified."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

repo=Path.cwd()
binary=str(repo/'target/debug/envira')
root=Path(tempfile.mkdtemp(prefix='envira-review-round2-'))
base={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_')) and k not in ('GOROOT','GOPATH')}
base.update(ENVIRA_NO_UPDATE_CHECK='1',ENVIRA_PRIVILEGE='none',PATH='/usr/bin:/bin')
def setup(name,cat):
    home=root/name
    home.mkdir()
    env=dict(base,HOME=str(home),ENVIRA_STATE_DIR=str(home/'state'),ENVIRA_CONFIG_DIR=str(home/'config'),ENVIRA_CACHE_DIR=str(home/'cache'))
    (home/'catalog.toml').write_text(cat)
    return home,env,[binary,'--no-remote','--no-overlay','--catalog',str(home/'catalog.toml')]
def run(argv,env,cwd=None):
    p=subprocess.run(argv,env=env,cwd=cwd,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print('EXIT:',p.returncode)
    print(p.stdout.replace(str(root),'<scratch>').replace(str(repo),'<repo>').strip())
    return p

# Actual Go install/update recipe; both the pre-existing binary and curl are explicit fixtures.
go=(repo/'catalog/toolchains.toml').read_text()
cat='[tools.curl]\nmanual="Synthetic offline download fixture"\n'+go[go.index('[tools.go]'):go.index('[tools.fnm]')]
home,env,cli=setup('go-failure',cat)
(home/'.go/bin').mkdir(parents=True)
p=home/'.go/bin/go'
p.write_text('#!/bin/sh\nprintf "go version go1.0.0 linux/amd64\\n"\n')
p.chmod(0o755)
(home/'fixtures').mkdir()
p=home/'fixtures/curl'
p.write_text('''#!/bin/sh
# Synthetic download response: successful transport, invalid archive body.
if [ "$1" = --version ]; then printf 'curl 1.0.0\n'; exit 0; fi
while [ $# -gt 0 ]; do
  if [ "$1" = -o ]; then printf 'invalid archive test data\n' > "$2"; exit 0; fi
  shift
done
printf 'go99.0.0\n'
''')
p.chmod(0o755)
env['PATH']=str(home/'fixtures')+':/usr/bin:/bin'
(home/'state').mkdir()
(home/'state/state.toml').write_text('''[tools.go]
method="run"
scope="user"
installed_at="2026-11-13T00:00:00Z"
last_result="ok"
''')
print('\nCASE: Invalid Go archive loses previous install')
run(cli+['update','go','--offline','-y'],env)
print('PREVIOUS GO BINARY STILL EXISTS:',(home/'.go/bin/go').exists())
print('\nCASE: Retry an update whose binary is gone')
run(cli+['retry','-y'],env)

# Exported scripts run relative file operations directly in the caller's working directory.
cat='''[tools.local]
bins=[]
detect={cmd='test -f "$HOME/installed"'}
install=[{run='printf changed > payload; touch "$HOME/installed"'}]
'''
home,env,cli=setup('script-cwd',cat)
work=home/'work'
work.mkdir()
(work/'payload').write_text('pre-existing user data')
print('\nCASE: Native execution uses a work directory')
run(cli+['install','local','-y'],env,cwd=work)
print('CALLER DATA AFTER NATIVE RUN:',(work/'payload').read_text())
# A second fresh home for the exported path: do not remove anything to reset a fixture.
home,env,cli=setup('script-export',cat)
work=home/'work'
work.mkdir()
(work/'payload').write_text('pre-existing user data')
print('\nCASE: Exported execution overwrites caller data')
p=run(cli+['plan','local','--script'],env,cwd=work)
script=home/'plan.sh'
script.write_text(p.stdout)
run(['bash',str(script)],env,cwd=work)
print('CALLER DATA AFTER EXPORTED RUN:',(work/'payload').read_text())

# Corrupt backup metadata must not silently turn the modified file into the original backup.
home,env,cli=setup('corrupt-backup','')
f=home/'file'
f.write_text('original user data')
print('\nCASE: Corrupt backup metadata')
run([binary,'fs','write',str(f),'--content','first change'],env)
meta=next((home/'state/backups').glob('*/meta.toml'))
meta.write_text('INVALID TOML TEST DATA = [')
run([binary,'fs','write',str(f),'--content','second change'],env)
run([binary,'fs','restore',str(f)],env)
print('RESTORED CONTENT:',f.read_text())

# The production line action checks only the first match; shell assignments use the last.
home,env,cli=setup('duplicate-lines','')
f=home/'settings.sh'
f.write_text('export DISABLE_UPDATE_PROMPT=true\nexport DISABLE_UPDATE_PROMPT=false\n')
print('\nCASE: Duplicate shell assignment is falsely verified')
run([binary,'fs','line','--check',str(f),'--match','^export DISABLE_UPDATE_PROMPT=','--line','export DISABLE_UPDATE_PROMPT=true'],env)
run([binary,'fs','line',str(f),'--match','^export DISABLE_UPDATE_PROMPT=','--line','export DISABLE_UPDATE_PROMPT=true'],env)
run(['bash','-c','. "$HOME/settings.sh"; printf "%s\\n" "$DISABLE_UPDATE_PROMPT"'],env)
print('\nScratch artifacts:',root)
```

### Bootstrap directory collisions

```python
"""Bootstrap collision fixture; serves only the current build over loopback HTTP."""
import functools
import hashlib
import http.server
import os
from pathlib import Path
import subprocess
import tempfile
import threading

repo=Path.cwd()
root=Path(tempfile.mkdtemp(prefix='envira-review-bootstrap-'))
site=root/'site'
site.mkdir()
binary=repo/'target/debug/envira'
asset='envira-x86_64-linux'
(site/asset).symlink_to(binary)
with binary.open('rb') as f:
    digest=hashlib.file_digest(f,'sha256').hexdigest()
(site/(asset+'.sha256')).write_text(digest+'  '+asset+'\n')
class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self,*args): pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(site)))
threading.Thread(target=server.serve_forever,daemon=True).start()
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root),ENVIRA_BASE_URL=f'http://127.0.0.1:{server.server_port}',ENVIRA_NO_LAUNCH='1')
for name in ['final-directory','staging-directory']:
    dest=root/name
    dest.mkdir()
    (dest/('envira' if name=='final-directory' else '.envira.new')).mkdir()
    env['ENVIRA_INSTALL_DIR']=str(dest)
    p=subprocess.run(['bash',str(repo/'envira.sh')],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print(name,'EXIT',p.returncode)
    print(p.stdout.replace(str(root),'<scratch>'))
    print('FINAL TARGET IS DIRECTORY:',(dest/'envira').is_dir())
    print('DOWNLOADED FILES:',[str(f.relative_to(dest)) for f in dest.rglob('*') if f.is_file()])
server.shutdown()
print('Scratch artifacts:',root)
```

### Overlapping TUI refreshes

```python
"""Synthetic version/delay fixtures, driven through the real TUI in a scratch HOME."""
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import tomllib

root=Path(tempfile.mkdtemp(prefix='envira-review-refresh-'))
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root), ENVIRA_STATE_DIR=str(root/'state'), ENVIRA_CONFIG_DIR=str(root/'config'), ENVIRA_CACHE_DIR=str(root/'cache'), ENVIRA_NO_UPDATE_CHECK='1', ENVIRA_PRIVILEGE='none', TERM='xterm-256color')
(root/'state').mkdir()
(root/'state/state.toml').write_text('''[tools.review]
method="run"
scope="user"
installed_at="2026-11-13T00:00:00Z"
last_result="ok"
''')
(root/'latest.sh').write_text('''#!/bin/sh
# The first request returns stale data after the refreshed request has completed.
if mkdir "$HOME/first-request" 2>/dev/null; then
  sleep 5
  printf 2.0.0
else
  touch "$HOME/second-request"
  printf 3.0.0
fi
''')
(root/'catalog.toml').write_text('''[tools.review]
bin="cat"
detect={cmd="printf 1.0.0"}
install=[{run="exit 99", update="exit 99", latest={cmd='sh "$HOME/latest.sh"'}}]
''')
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,150,0,0))
p=subprocess.Popen([str(Path.cwd()/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(root/'catalog.toml')],env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
os.close(slave)
output=bytearray()
def drain(seconds):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if select.select([master],[],[],min(.05,max(0,end-time.monotonic())))[0]:
            try: output.extend(os.read(master,65536))
            except OSError: break
try:
    deadline=time.monotonic()+10
    while not (root/'first-request').exists() and time.monotonic()<deadline:
        drain(.1)
    assert (root/'first-request').exists(), 'Initial latest request did not start'
    os.write(master,b'R')
    drain(1)
    cache=root/'state/latest.toml'
    print('SECOND REQUEST EXECUTED:',(root/'second-request').exists())
    print('CACHE AFTER REFRESH:',cache.read_text() if cache.exists() else 'missing')
    drain(5)
    print('CACHE AFTER OLDER REQUEST RETURNS:',cache.read_text())
    os.write(master,b'q')
    drain(.5)
    (root/'screen.bin').write_bytes(output)
    print('Scratch artifacts:',root)
finally:
    if p.poll() is None: p.send_signal(signal.SIGTERM)
    try: p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
    os.close(master)
```

### Privilege boundary reproduction (container only)

```python-container
"""Synthetic fixtures; this script is run only inside a disposable offline container."""
import os
from pathlib import Path
import subprocess

cat=Path('/tmp/review-catalog.toml')
cat.write_text('''[configs.review-system]
scope="system"
check='{envira} fs write --check /etc/envira-review.conf --content new'
apply='{sudo} {envira} fs write /etc/envira-review.conf --content new'
revert='{sudo} {envira} fs restore /etc/envira-review.conf'
''')
Path('/etc/envira-review.conf').write_text('old')
cli=['/opt/envira-review','--no-remote','--no-overlay','--catalog',str(cat)]
base=['runuser','-u','user','--','env','HOME=/home/user','ENVIRA_NO_UPDATE_CHECK=1']
for title,args in [
    ('apply as user through sudo',base+cli+['config','apply','review-system','-y']),
    ('revert through sudo envira',base+['sudo','env','ENVIRA_NO_UPDATE_CHECK=1']+cli+['config','revert','review-system','-y']),
]:
    p=subprocess.run(args,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print(title,'EXIT',p.returncode)
    print(p.stdout)
print('CONFIG CONTENT:',Path('/etc/envira-review.conf').read_text())
print('ROOT BACKUP EXISTS:',bool(list(Path('/root/.local/state/envira/backups').glob('*/meta.toml'))))
print('USER SYSTEM BACKUP EXISTS:',any('envira-review.conf' in p.read_text() for p in Path('/home/user/.local/state/envira/backups').glob('*/meta.toml')))
```
