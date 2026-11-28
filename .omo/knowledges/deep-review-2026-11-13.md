# Deep review of envira 0.1.5 (2026-11-13)

**Status (2026-11-28):** every finding below was fixed in `573f824`, and the gaps the fix verification found (F1-F7) in `dca939d`; both shipped in 0.1.6. See `deep-review-fix-verification-2026-11-13.md`.

Reviewed commit: `491f7e3`. This was a review, not a remediation change. Production source,
catalog entries, project rules and real user configuration were not modified.

## Verification and scope

- `cargo test --locked`: 269 passed, 0 failed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo build --locked`: passed; reproductions used this fresh debug binary.
- `python3 -m unittest discover -s test -v`: 11 passed, using only the standard library.
- `bash -n envira.sh run.sh run_user.sh verify.sh tests/integration/*.sh`: passed.
- `ENVIRA_NO_UPDATE_CHECK=1 target/debug/envira --no-remote --no-overlay catalog check`:
  100 tools, 19 configs, 14 bundles, 12 methods, 7 actions; 0 errors and 0 warnings.

Reviewed the planner, execution/state/retry flow, config editing and backups, shell generation,
route adoption, versions, catalog/cache handling, download/self-update paths, TUI orchestration,
bootstrap and release workflows. The complete online distro installation matrix, native macOS
execution and current vendor endpoints were not exercised. No upstream endpoint behavior is
claimed from these local tests.

All fixtures below are **synthetic test data**, not production implementations or real
credentials. Writes are confined to fresh temporary HOME/state/config/cache directories.
The Go check invokes the real Go executable already on PATH. The cache check uses a local HTTP
server; no vendor installers are executed. Fixtures intentionally leave their scratch artifacts
for inspection. The TUI test uses a pseudo-terminal and the real application.

## Confirmed findings

### R1 — P1: backup metadata exposes values from private files

Locations: `src/fs.rs:94`, `src/fs.rs:163`, `src/files.rs:79`.

`record_key` saves the old value in `meta.toml`, while `Backups::save` passes no restrictive mode.
Only `orig` inherits the original file's mode. A 0600 INI file produced a 0600 `orig`, a 0664
`meta.toml` containing the private test marker, and a 0775 backup slot under the review host's
umask. With umask 022 the corresponding defaults would be 0644/0755. If the parent directories
are traversable by another user, the original file's confidentiality is lost. A private HOME
can mask this, but must not be the backup implementation's protection.

Fix direction: explicitly enforce 0600 on metadata and 0700 on backup directories, including
existing backup metadata when saved. Test prior values in metadata, not only `orig` mode.

### R2 — P1: revert undoes steps envira never performed

Locations: `src/exec.rs:700`, `src/plan.rs:976`, `catalog/shell.toml:34`.

Passing checks are skipped on apply, but normal revert runs every revert command, with no
persistent distinction between a pre-existing resource and one envira created. Reproduced with
the real `zsh-theme` config: an existing theme file was skipped on apply, deleted on revert,
and the revert exited 0. The real `git-settings` config also fails to revert when the alias was
already correct: only `credential.helper` has a backup; after restoring it, restoring the
untouched alias fails with `no envira backup`.

Fix direction: record which steps actually changed resources, with stable step identities and
prior-state/ownership data. Revert only owned changes. A passing check is not ownership proof.
This also matters for pre-existing group membership and default-shell restoration.

### R3 — P2: a failed or drifted config cannot be reverted normally

Location: `src/plan.rs:960`.

Normal revert requires all current apply checks to pass (`present`). A two-step config whose
first step writes a file and whose second step fails is therefore skipped as `not applied` by
`config revert`; the first change remains. A previously successful config that drifts has the
same gate. `retry` of an apply retries applying it; it does not provide rollback of the partial
apply.

Fix direction: use recorded completed changes/backups to determine revert eligibility and
support partially applied and drifted states explicitly.

### R4 — P1: mixed TUI install/update plans lose dependency edges

Locations: `src/tui/app.rs:612`, `src/plan.rs:183`, `src/plan.rs:920`.

`make_selection_plan` builds updates and installs independently and concatenates them. Each
planner only adds edges to units in its own plan. An already-present dependency being updated
is omitted from the install subplan and its edges are lost. Array order does not serialize the
parallel executor.

Reproduced through the real TUI: select outdated `a-core` and absent `b-child` depending on it.
The core update takes two seconds; the child requires its completion marker. Result:
`a-core/update/ok`, `b-child/install/failed`; the core finished but the child started too soon.
The same mechanism can affect a Node update and a newly installed npm CLI, whose lanes differ.

Fix direction: compose targets before resolving a single DAG, or rebuild all cross-plan edges
and deduplicate units after merging. Check mixed-action dependencies as executor behavior.

### R5 — P2: shell generation breaks an external Go installation

Locations: `catalog/toolchains.toml:38`, `catalog/toolchains.toml:42`, `src/shellenv.rs:69`,
`src/exec.rs:1051`.

Any detected Go causes `GOROOT=~/.go` and `GOPATH=~/go` to be emitted, regardless of its install
route or existing environment. In a temporary HOME with the real host Go accessible on PATH,
`go version` succeeded before `envira env`; after sourcing the generated environment it exited
2 with `go: cannot find GOROOT directory: <scratch>/.go`. Ordinary runs also regenerate this
environment, so explicitly installing Go is not required to trigger the change.

Fix direction: avoid forcing GOROOT (Go can locate its own distribution) or gate route-specific
variables on the actual owned install location. Preserve an explicit existing GOPATH.

### R6 — P2: distribution package revisions disappear from update detection

Locations: `catalog/templates.toml:64`, `catalog/templates.toml:75`,
`catalog/templates.toml:87`, `src/version.rs:8`.

APT/pacman regexes remove release revisions and epochs; RPM queries use VERSION without
RELEASE/EPOCH. The numeric comparator also ignores suffix distinctions. Using the production
APT regexes with synthetic installed `1.2.3-1` and candidate `1.2.3-2` resulted in both versions
being `1.2.3`, status `installed`, and update saying everything was up to date. Distribution
patches that retain an upstream version cannot be detected by this representation.

Fix direction: keep full package-manager version identities and compare using the manager's
semantics or upgrade-query results. Retaining suffixes alone will not fix the numeric comparator.
Test revision-only, epoch and prerelease transitions.

### R7 — P2: generated PATH does not put an existing user directory first

Locations: `src/shellenv.rs:60`, `src/status.rs:254`.

The generated `envira_path_prepend` leaves a directory at its current position if it occurs
anywhere on PATH. Detection instead prepends catalog directories before the inherited PATH.
With `system-bin:/usr/bin:/bin:~/.local/bin`, sourcing the generated environment still ran the
system fixture, although envira's detection would prefer the user copy. This breaks the
user-mode update promise that the new user copy overrides the system binary.

Fix direction: define one PATH precedence policy for scanning and interactive shells, move
managed directories to their required position, and deduplicate while preserving that order.

### R8 — P2: home-directory location is insufficient evidence for route adoption

Locations: `src/plan.rs:382`, `src/status.rs:466`.

For routes whose install template does not use `{bin_dir}`, `path_route` accepts any binary
under HOME once dependencies are present. With a synthetic executable at `~/go/bin/fzf` and
the real bundled fzf entry, status reported `adoptable`; `update fzf --offline --dry-run`
planned `git -C "$HOME/.fzf" pull ...`, although that directory did not exist. This updates the
wrong installation layout. Similar raw routes require equally specific provenance checks.

Fix direction: declare and verify route-specific installed paths/metadata. Keep an installation
external when the source cannot be established; do not infer source from the HOME prefix.

### R9 — P2: shell-environment failures do not affect success or retry state

Locations: `src/exec.rs:430`, `src/commands.rs:1111`, `src/commands.rs:1140`.

Shell generation errors are only emitted as display events. They are absent from run units
and the exit-code computation. With a directory at the generated `env.sh` destination, a local
config apply printed `shell environment not updated ... Is a directory` yet exited 0 and
reported `1/1 succeeded`. Retry has no failed unit to recover this work.

Fix direction: persist shell generation as a run stage, reflect its failure in the command
outcome, and allow retry or explicitly surface a partial-success result to automation.

### R10 — P2: remote catalog cache is not bound to its source URL

Locations: `src/app.rs:236`, `src/catalog/remote.rs:44`.

Every `ENVIRA_CATALOG_URL` uses the same `catalog.toml` and ETag. Switching from local source A
to B within the cache TTL returned A's catalog without requesting B; the source label is built
from B's URL. Expired cache/ETag and offline fallback likewise lack source provenance.

Fix direction: key cache and validators by normalized source URL, or persist and check the
source before either cache reuse or fallback. Test source switches and redirects deliberately.

## Design follow-ups from code inspection

- `ToolRecord` stores method/scope but no durable route identity or installed path;
  `route_for` picks the first currently usable entry with the method name. All raw entries
  share `run`. Custom catalogs or reordered remote entries can therefore change a managed
  tool's update route. The current bundled catalog has no duplicate method names within a
  tool; this is an extensibility/remote-evolution issue, separate from the reproduced R8.
- `catalog/shell.toml:63` reverts the login shell to `/bin/bash`, without recording the original
  shell. It cannot restore a former fish or other shell. This is another consequence of
  treating a declared reverse command as a prior-state restoration.
- The bundled `git-settings` changes the credential helper to `store`. Preserve an existing
  helper by default or make this a separate opt-in setting; grouping it with an alias change
  gives it too little visibility. No real credentials were inspected in this review.
- Online install recipes, such as replacing `~/.go` after a download, still need fault-injection
  coverage for invalid archives, low disk space and interruption. These were not executed in
  this review, so no additional reproduced failure is claimed here.

## Reproduce the review

Run from the repository root with the fresh debug binary and Python standard library:

```sh
cargo build --locked
python3 - <<'PY'
from pathlib import Path
import re
import subprocess
import sys
note = Path('.omo/knowledges/deep-review-2026-11-13.md').read_text()
for code in re.findall(r'```python\n(.*?)\n```', note, re.S):
    subprocess.run([sys.executable, '-c', code], check=True)
PY
```

These are diagnostic reproductions, not a passing regression suite: the command intentionally
prints the buggy outcomes listed above and leaves temporary artifacts. The Go example requires
a working Go on PATH; the TUI example requires Linux/Unix pseudo-terminal support and uses short
settling delays. Use test values only. Never copy real private settings into these fixtures.

### CLI, backups, versions and shell generation

```python
"""Review-only synthetic fixtures; never use real credentials or install software."""
import json
import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import tempfile
import tomllib

REPO = Path.cwd()
BIN = str(REPO / 'target/debug/envira')
ROOT = Path(tempfile.mkdtemp(prefix='envira-review-'))
BASE = dict(os.environ)
for key in list(BASE):
    if key.startswith(('ENVIRA_', 'SUDO_', 'XDG_')) or key in ('GOROOT', 'GOPATH'):
        BASE.pop(key, None)
BASE.update(ENVIRA_NO_UPDATE_CHECK='1', ENVIRA_PRIVILEGE='none')

def setup(name, catalog):
    home = ROOT / name
    home.mkdir()
    env = dict(BASE, HOME=str(home), ENVIRA_STATE_DIR=str(home/'state'),
               ENVIRA_CONFIG_DIR=str(home/'config'), ENVIRA_CACHE_DIR=str(home/'cache'))
    cat = home/'catalog.toml'
    cat.write_text(catalog)
    return home, env, [BIN, '--no-remote', '--no-overlay', '--catalog', str(cat)]

def run(argv, env):
    return subprocess.run(argv, env=env, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, timeout=40)

def show(title, result):
    print('\nCASE:', title, 'EXIT:', result.returncode)
    print(result.stdout.replace(str(ROOT), '<scratch>').strip())

# Use production action templates and the actual bundled git-settings config.
templates = (REPO/'catalog/templates.toml').read_text()
shell = (REPO/'catalog/shell.toml').read_text()
git_config = shell[shell.index('[configs.git-settings]'):shell.index('[bundles.shell]')]
cat = templates + '\n[tools.git]\nmanual="Provided by the test host"\n' + git_config
home, env, cli = setup('partial', cat)
(home/'.gitconfig').write_text('[alias]\nlsd = log --graph --decorate --pretty=oneline --abbrev-commit --all\n[credential]\nhelper = cache\n')
show('partial git config apply', run(cli+['config','apply','git-settings','-y'], env))
show('partial git config revert', run(cli+['config','revert','git-settings','-y'], env))

# Partial failed apply: one real file edit succeeds and the second command intentionally fails.
cat = '''[configs.partial]
steps = [
 { check = 'test -f "$HOME/first"', run = 'touch "$HOME/first"', revert = 'rm -f "$HOME/first"' },
 { check = 'test -f "$HOME/second"', run = 'exit 1', revert = 'rm -f "$HOME/second"' },
]
'''
home, env, cli = setup('failed-apply', cat)
show('failed apply', run(cli+['config','apply','partial','-y'], env))
show('revert failed apply', run(cli+['config','revert','partial','-y'], env))
print('FIRST CHANGE STILL EXISTS:', (home/'first').exists())

# Synthetic private value: no credential is read or written.
home, env, cli = setup('private', '')
f = home/'private.ini'
f.write_text('[section]\nsetting = PRIVATE_TEST_SENTINEL_NOT_A_CREDENTIAL\n')
f.chmod(0o600)
r = run([BIN,'fs','ini-set',str(f),'section','setting','new-test-value'],env)
show('private config key edit',r)
for p in sorted((home/'state/backups').rglob('*')):
    print('BACKUP MODE:', oct(stat.S_IMODE(p.stat().st_mode)), p.name if p.is_file() else '<directory>')
    if p.name == 'meta.toml':
        print('PRIOR VALUE IN METADATA:', 'PRIVATE_TEST_SENTINEL' in p.read_text())

# Production Go shell definition, with the real host Go binary detected from PATH.
go_text = (REPO/'catalog/toolchains.toml').read_text()
go_entry = go_text[go_text.index('[tools.go]'):go_text.index('[tools.fnm]')]
cat = '[tools.curl]\nmanual="Provided by the test host"\n' + go_entry
home, env, cli = setup('external-go',cat)
show('real Go before shell environment',run(['go','version'],env))
show('generate Go shell environment',run(cli+['env'],env))
show('real Go after sourcing generated environment',run(['bash','-c','. "$ENVIRA_CONFIG_DIR/env.sh"; go version'],env))

# Production PATH-generation behavior; two test executables identify PATH precedence.
cat = '[env]\nshell_path=["~/.local/bin"]\n'
home, env, cli = setup('path',cat)
for d, label in [('system-bin','SYSTEM_TEST_FIXTURE'),('.local/bin','USER_TEST_FIXTURE')]:
    path = home/d
    path.mkdir(parents=True)
    exe = path/'review-path-tool'
    exe.write_text('#!/bin/sh\nprintf "%s\\n" '+shlex.quote(label)+'\n')
    exe.chmod(0o755)
env['PATH'] = str(home/'system-bin')+':/usr/bin:/bin:'+str(home/'.local/bin')
show('generate PATH environment',run(cli+['env'],env))
show('PATH precedence after source',run(['bash','-c','. "$ENVIRA_CONFIG_DIR/env.sh"; review-path-tool'],env))

# Production apt version regex against synthetic package-manager responses.
apt = tomllib.loads(templates)['methods']['apt']
cat = '''[tools.review-version]
bin = "printf"
detect = {cmd="printf '1.2.3'"}
install = [{method="review-apt"}]
[methods.review-apt]
install = "exit 99"
installed = {cmd="printf '1.2.3-1'", regex=REGEX_INSTALLED}
latest = {cmd="printf '  Candidate: 1.2.3-2'", regex=REGEX_LATEST}
'''.replace('REGEX_INSTALLED',json.dumps(apt['installed']['regex'])).replace('REGEX_LATEST',json.dumps(apt['latest']['regex']))
home, env, cli = setup('package-revision',cat)
show('package revision status',run(cli+['status','--latest','--json'],env))
show('package revision update',run(cli+['update','--offline','--dry-run'],env))

# A broken shell-environment destination after a successful local config apply.
cat = '''[configs.local]
check = 'test -f "$HOME/done"'
apply = 'touch "$HOME/done"'
'''
home,env,cli = setup('shell-error',cat)
(home/'config').mkdir()
(home/'config/env.sh').mkdir()
show('shell generation failure exit code',run(cli+['config','apply','local','-y'],env))
print('\nScratch artifacts:', ROOT)
```

### TUI mixed install/update dependency

```python
"""Synthetic local update/dependency fixture, driven through the real TUI."""
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

root=Path(tempfile.mkdtemp(prefix='envira-review-tui-'))
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root), ENVIRA_STATE_DIR=str(root/'state'), ENVIRA_CONFIG_DIR=str(root/'config'), ENVIRA_CACHE_DIR=str(root/'cache'), ENVIRA_NO_UPDATE_CHECK='1', ENVIRA_PRIVILEGE='none', TERM='xterm-256color')
(root/'core-version').write_text('1.0.0')
(root/'state').mkdir()
(root/'state/state.toml').write_text('''[tools.a-core]
method="run"
scope="user"
installed_at="2026-11-13T00:00:00Z"
last_result="ok"
''')
(root/'catalog.toml').write_text('''[tools.a-core]
bin="cat"
detect={cmd='cat "$HOME/core-version"'}
install=[{run='exit 99', update='sleep 2; printf 2.0.0 > "$HOME/core-version"; touch "$HOME/update-done"', latest={cmd="printf 2.0.0"}}]
[tools.b-child]
bins=[]
depends=["a-core"]
detect={cmd='test -f "$HOME/child"'}
install=[{run='test -f "$HOME/update-done" && touch "$HOME/child"'}]
''')
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,150,0,0))
p=subprocess.Popen([str(Path.cwd()/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(root/'catalog.toml')],env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
os.close(slave)
output=bytearray()
def drain(seconds):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if select.select([master],[],[],min(.1,max(0,end-time.monotonic())))[0]:
            try: output.extend(os.read(master,65536))
            except OSError: break
try:
    drain(3)
    os.write(master,b'lng  \r')
    drain(1)
    os.write(master,b'\r')
    drain(5)
    os.write(master,b'qq')
    drain(1)
    if p.poll() is None:
        p.send_signal(signal.SIGTERM)
        drain(4)
    for f in (root/'state/logs').glob('*/run.toml'):
        record=tomllib.loads(f.read_text())
        print([(u['id'],u['action'],u['outcome'],u.get('detail')) for u in record['units']])
    print('UPDATE FINISHED:', (root/'update-done').exists())
    print('CHILD INSTALLED:', (root/'child').exists())
    (root/'screen.bin').write_bytes(output)
    print('Scratch artifacts:',root)
finally:
    if p.poll() is None: p.kill()
    p.wait()
    os.close(master)
```

### Adoption and catalog cache source

```python
"""Synthetic fixtures for route adoption and cache provenance; no installers run."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

repo=Path.cwd()
root=Path(tempfile.mkdtemp(prefix='envira-review-extra-'))
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root), ENVIRA_STATE_DIR=str(root/'state'), ENVIRA_CONFIG_DIR=str(root/'config'), ENVIRA_CACHE_DIR=str(root/'cache'), ENVIRA_NO_UPDATE_CHECK='1', ENVIRA_PRIVILEGE='none')
cli=[str(repo/'target/debug/envira'),'--no-overlay']
text=(repo/'catalog/terminal.toml').read_text()
fzf=text[text.index('[tools.fzf]'):text.index('[tools.lazygit]')]
(root/'catalog.toml').write_text('[tools.git]\nmanual="Host git for isolated test"\n'+fzf)
(root/'go/bin').mkdir(parents=True)
exe=root/'go/bin/fzf'
exe.write_text('#!/bin/sh\nprintf "0.60.0\\n"\n')
exe.chmod(0o755)
env['PATH']=str(root/'go/bin')+':/usr/bin:/bin'
for args in [['status','--json'],['update','fzf','--offline','--dry-run']]:
    out=subprocess.run(cli+['--no-remote','--catalog',str(root/'catalog.toml')]+args,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20)
    print('ADOPTION',args,'EXIT',out.returncode)
    if args[0]=='status':
        item=next(i for i in json.loads(out.stdout) if i['id']=='fzf')
        print({k:item[k] for k in ('id','ownership','path','route')})
    else: print(out.stdout.replace(str(root),'<scratch>'))
print('EXPECTED UPDATE DIRECTORY EXISTS:',(root/'.fzf').exists())

requests=[]
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        requests.append(self.path)
        body=('[tools.review]\nmanual="Test source '+self.path+'"\n').encode()
        self.send_response(200)
        self.send_header('Content-Length',str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self,*args): pass
server=http.server.HTTPServer(('127.0.0.1',0),Handler)
threading.Thread(target=server.serve_forever,daemon=True).start()
for source in ['A','B']:
    env['ENVIRA_CATALOG_URL']=f'http://127.0.0.1:{server.server_port}/{source}'
    out=subprocess.run(cli+['catalog','show'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20)
    print('REQUESTED SOURCE:',source,'CONTENT:',out.stdout.strip())
print('HTTP REQUESTS:',requests)
server.shutdown()
print('Scratch artifacts:',root)
```

### Reverting a pre-existing theme

```python
"""A pre-existing theme is synthetic user data; the real bundled config must preserve it."""
import os
from pathlib import Path
import subprocess
import tempfile

repo=Path.cwd()
root=Path(tempfile.mkdtemp(prefix='envira-review-theme-'))
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root), ENVIRA_STATE_DIR=str(root/'state'), ENVIRA_CONFIG_DIR=str(root/'config'), ENVIRA_CACHE_DIR=str(root/'cache'), ENVIRA_NO_UPDATE_CHECK='1', ENVIRA_PRIVILEGE='none')
shell=(repo/'catalog/shell.toml').read_text()
config=shell[shell.index('[configs.zsh-theme]'):shell.index('[configs.zsh-plugins]')]
cat=(repo/'catalog/templates.toml').read_text()+'''
[tools.oh-my-zsh]
bins=[]
manual="Isolated local test fixture"
detect={cmd='test -f "$HOME/.oh-my-zsh/oh-my-zsh.sh"'}
[tools.curl]
manual="Provided by the host"
'''+config
(root/'catalog.toml').write_text(cat)
(root/'.oh-my-zsh/themes').mkdir(parents=True)
(root/'.oh-my-zsh/oh-my-zsh.sh').write_text('# Test fixture\n')
theme=root/'.oh-my-zsh/themes/mzz-ys.zsh-theme'
theme.write_text('# Pre-existing custom theme test data\n')
(root/'.zshrc').write_text('ZSH_THEME="old-theme"\n')
cli=[str(repo/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(root/'catalog.toml')]
for action in ['apply','revert']:
    out=subprocess.run(cli+['config',action,'zsh-theme','-y'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20)
    print(action, 'EXIT',out.returncode, 'PREEXISTING THEME EXISTS',theme.exists())
    print(out.stdout.replace(str(root),'<scratch>'))
print('Scratch artifacts:',root)
```
