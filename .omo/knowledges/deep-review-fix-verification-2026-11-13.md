# Verification of the deep-review fixes (2026-11-13)

**Status (2026-11-28):** F1-F7 were fixed in `dca939d` and shipped in 0.1.6.

Reviewed HEAD: `09c1757`, compared with original review baseline `491f7e3`.
Changes include `573f824`, `5a59bad`, `c096364`, and `09c1757`.
This is a verification review, not another implementation patch. No production source,
catalog, project rules, real user configuration or installed tools was modified.

Conclusion: many original reproductions now behave correctly, but **seven confirmed gaps
remain: F1 is P1; F2-F7 are P2**. Original R numbers below refer to
`deep-review-2026-11-13-all-findings.md`; that document is a historical inventory rather than
the current closure status. Passing an original fixture does not prove all variants fixed.

## Checks actually run

```sh
cargo test --locked
# 288 passed, 0 failed.
cargo build --locked
# Passed; all application reproductions below use this rebuilt binary.
cargo clippy --locked --all-targets -- -D warnings
# Passed.
cargo fmt --all -- --check
# Passed.
python3 -m unittest discover -s test -v
# 11 passed; Python standard library only.
bash -n envira.sh run.sh run_user.sh verify.sh tests/integration/*.sh
# Passed.
ENVIRA_NO_UPDATE_CHECK=1 target/debug/envira --no-remote --no-overlay catalog check
# 100 tools, 20 configs, 14 bundles, 12 methods, 7 actions; 0 errors, 0 warnings.
```

Replayed all nine host-side diagnostic script blocks from the three earlier notes and the
container-only sudo script. They completed. These scripts print observations rather than
asserting closure, so their exit codes alone are not proof of a fix. Inspected the resulting
state, files, logs and TUI observations, and ran the additional fixtures below.

All custom commands, package responses and failure conditions are explicitly **synthetic test
fixtures**, scoped to temporary HOME/state/config/cache directories. The crash test deliberately
kills only its own newly created process group after a test marker appears. The Go test uses
the host Go executable with a temporary HOME; it neither installs Go nor contacts a vendor.
Docker tests use existing disposable images with `--network none`, read-only source/binary
mounts and a synthetic account named `user`. No real credentials are used.

## Remaining findings

### F1 — P1: config step identity changes with the execution entrypoint

Locations: `src/plan.rs:165`, `src/plan.rs:1093`, `src/plan.rs:1100`, `src/exec.rs:1075`.
Related original findings: R14, R21, and the changed-resource tracking introduced for R2/R3.

`step_key` hashes the expanded reverse command. That command contains execution details:
the absolute envira executable path, sudo, and the state-directory environment wrapper.
These are not stable resource identities. Running the same config from a different binary
path or switching normal-user/sudo entrypoints yields different keys.

Two reproductions confirmed the consequence:

- Apply through the actual build, then revert through a hard link to the exact same binary
  at another path. Revert exited 0 with an empty unit; the file still held `new`, but state
  became `applied=false`, `changed=[]`, `last_result=ok`.
- The original offline-container R14 fixture now stores its backup in the correct user
  namespace, but normal apply followed by `sudo envira` revert exits 0 without restoring
  `/etc/envira-review.conf`. It still contains `new`.

Fix direction: identify the logical resource/action independently of binary path and privilege
wrappers. Persist enough original operation data to restore it. If recorded keys cannot be
resolved, report incomplete recovery and retain them; an empty filtered plan must not silently
clear outstanding changes.

### F2 — P2: a crash during apply loses ownership of changes already made

Locations: `src/exec.rs:487`, `src/exec.rs:520`, `src/exec.rs:559`, `src/plan.rs:1058`.
Related original finding: R3; this is an unhandled crash boundary of the new ownership model.

Only revert progress is journaled. Apply's `ran` keys stay in memory until the unit finishes.
The fixture created its applied marker and then waited. Killing that fixture's process group
left a pending run but no ConfigRecord. `retry` exited 0 and skipped the config as already
present; normal revert then refused with `envira did not change it`. The marker remained.

Fix direction: durably record apply intent/progress before side effects and reconcile in-flight
steps after restart. A pending apply cannot be discharged solely because its presence check
passes; the operation still needs its ownership/recovery record.

### F3 — P2: a missing recorded install route silently falls back to another route

Location: `src/plan.rs:438`. Related original finding: R19.

The new entry key fixes reordering while the old entry is still present. If its key no longer
matches, `route_for` falls back to the first usable entry with the same method. All raw entries
are still `run`. The fixture installed through A, removed A from its catalog, and updated:
the actual updater was B and the command exited 0. Editing A's hashed install command can
also invalidate the key; that variant was not separately executed.

Fix direction: distinguish legacy records with no entry identity from records whose known
entry disappeared. The latter must not silently change installer; preserve identity and
require a deliberate migration or expose an unsupported route.

### F4 — P2: directory existence is insufficient to set GOROOT

Location: `src/shellenv.rs:89`. Related original finding: R5.

The new directory guard fixes a missing `~/.go`, but still overrides a working external Go
when that directory happens to exist. With an empty temporary `~/.go`, `go list fmt` succeeded
before sourcing generated env.sh and failed afterwards:
`package fmt is not in std (<scratch>/.go/src/fmt)`.
`go version` alone succeeds in this case and is therefore an insufficient regression check.

Fix direction: avoid overriding Go's inferred GOROOT, or bind the value to the actual selected
Go executable/installation. Preserve a user's explicit GOPATH rather than deciding its value
from the existence of a directory.

### F5 — P2: several managed PATH entries still disagree with detection order

Locations: `src/shellenv.rs:83`, `src/status.rs:273`. Related original finding: R7.

Moving an existing path entry to the front fixes the original single-directory example.
However, render prepends each declared directory in forward order, reversing their priority,
whereas detection accumulates directories in forward order. Two declared fixture directories
each supplied `review-bin`: status selected `~/.a/bin/review-bin`, while a shell sourcing the
generated environment selected `~/.b/bin/review-bin`.

Fix direction: derive both paths from one ordered policy. Test collisions between multiple
managed directories, including the interaction with global env.path and env.shell_path.

### F6 — P2: RPM package versions are compared using dpkg semantics

Locations: `src/version.rs:38`, `src/status.rs:123`. Related original finding: R6.

Whole package revisions are now retained and the APT 1.2.3-1 to 1.2.3-2 example is correctly
outdated. But all four package methods use one dpkg-style comparator. A synthetic dnf method
with installed `1.0^git-1` and latest `1.0.1-1` was reported `installed` rather than outdated.
The real RPM comparator in the existing Fedora container returned 1 for latest versus current:

```sh
docker run --rm --network none --user root envira-it-fedora \
  rpm --eval '%{lua:print(rpm.vercmp("1.0.1-1", "1.0^git-1"))}'
# Observed: 1 (the candidate is newer).
```

Fix direction: choose version semantics by the actual package manager. RPM's caret handling
is not dpkg ordering; retaining the complete version string alone does not solve comparison.

### F7 — P2: TUI cannot retry a shell-environment-only failure

Locations: `src/tui/app.rs:1505`, `src/tui/app.rs:1543`, `src/tui/app.rs:1673`.
Related original finding: R9.

CLI now saves `shell_env_error`, exits 1 and can regenerate through retry. TUI retry targets
still consider only unfinished units. The fixture made config apply succeed but env.sh
generation fail. After moving the temporary blocker aside, starting the real TUI and pressing
`t` displayed `The last run left nothing to retry`; env.sh remained absent. CLI `retry -y`
then exited 0 and created it. The run-screen `r` path uses the same unit-only model.

Fix direction: represent shell regeneration in the TUI retry request, including loading the
last run and retrying directly from the run screen. Do not drop a persisted failure because
all tool/config units succeeded.

## Original-finding disposition

Each entry states the tested scope, not a blanket guarantee for all possible inputs.

| Original | Verification result |
| --- | --- |
| R1 | Original private-file test passes: slot 0700 and metadata 0600. |
| R2 | Original pre-existing-theme test passes: the file survives revert. Legacy state migration was not comprehensively exercised. |
| R3 | Ordinary partial failure now reverts its first change. Crash recovery still fails (F2). |
| R4 | Original mixed TUI test passes: update and dependent install both succeed. |
| R5 | Missing-layout example passes; existing unrelated layout fails (F4). |
| R6 | Real method name `apt` now detects revision-only update; RPM semantics fail (F6). The old fixture's custom `review-apt` name is not in the new built-in classification and is not evidence against the bundled APT fix. |
| R7 | Original user-directory precedence passes; multiple managed directories fail (F5). |
| R8 | Original `~/go/bin/fzf` case is now external and is not updated through `~/.fzf`. No blanket provenance guarantee is claimed for every raw route. |
| R9 | CLI records the error, exits 1 and retries successfully; TUI recovery still fails (F7). |
| R10 | Two local source URLs return their own catalogs. |
| R11 | Invalid archive test leaves the old Go binary intact. Interruption during the directory swap was not injected. |
| R12 | Invalid metadata blocks the second edit and restore; the original backup is preserved rather than overwritten. |
| R13 | Exported script now preserves caller data in the working-directory fixture. Full user/environment equivalence was not proven. |
| R14 | Backup directory routing fixed, but cross-entrypoint revert silently does nothing (F1). |
| R15 | Separate synthetic update removes its marker and fails; retry now reinstalls successfully. |
| R16 | Late old TUI query no longer replaces version 3.0.0 with 2.0.0 in the original fixture. |
| R17 | Conflicting duplicate assignments now fail check and refuse edit. |
| R18 | Final-directory collision exits 1; old fixed staging-directory collision installs a real file using unique staging. |
| R19 | Reordering existing entries preserves route A; removing A still runs B (F3). |
| R20 | Key restore keeps the outstanding whole-file backup; the following whole restore returns original content. |
| R21 | Original reordered-step retry now restores both settings. Step identity across execution entrypoints remains broken (F1). |
| R22 | Missing reverse command now exits 1 and keeps applied state. |
| R23 | Offline Ubuntu test now reports latest=null with an explicit stale-index error. An explicit update dry run retains an update unit containing apt-refresh instead of skipping it as current. This verifies the freshness guard/planning, not an actual live-repository upgrade or automatic global refresh. |
| D1 | Same-entrypoint test in an offline disposable container restored `/bin/sh` after apply changed it to zsh. Cross-entrypoint caveat F1 still applies. |
| D2 | Source inspection confirms a separate git-credential-store item, absent from full/user/shell; included only in opt-in extras. |

## Replaying the additional host-side evidence

Linux, Python 3.11+ standard library, and a working external Go are needed. No third-party
Python environment or package installation is involved. The first three Python blocks below
run on the host; the container-only block must not run on the host.

```sh
cargo build --locked
python3 - <<'PY'
from pathlib import Path
import re
import subprocess
import sys
import tempfile
note = Path('.omo/knowledges/deep-review-fix-verification-2026-11-13.md').read_text()
out = Path(tempfile.mkdtemp(prefix='envira-fix-review-replay-'))
for i, source in enumerate(re.findall(r'^```python\n(.*?)^```', note, re.M | re.S), 1):
    script = out / f'case-{i}.py'
    script.write_text(source)
    subprocess.run([sys.executable, str(script)], check=True)
PY
```

Expected remaining-bug signals: revert leaves `new` with `changed=[]`; interrupted apply stays
present but unowned; install/update routes are A/B; Go cannot find fmt after shell init;
detection and shell choose different fixture paths; dnf caret transition remains installed;
TUI says nothing to retry, followed by successful CLI recovery. Positive controls include
APT revision detection and missing-tool retry recovery.

### Execution, recovery, routes and environment boundaries

```python
"""Synthetic post-fix boundary fixtures; only fresh temporary homes are modified."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import tomllib

repo=Path.cwd()
binary=repo/'target/debug/envira'
root=Path(tempfile.mkdtemp(prefix='envira-fix-boundaries-'))
base={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_')) and k not in ('GOROOT','GOPATH')}
base.update(ENVIRA_NO_UPDATE_CHECK='1',ENVIRA_PRIVILEGE='none')
def setup(name,cat):
    home=root/name
    home.mkdir()
    env=dict(base,HOME=str(home),ENVIRA_STATE_DIR=str(home/'state'),ENVIRA_CONFIG_DIR=str(home/'config'),ENVIRA_CACHE_DIR=str(home/'cache'))
    (home/'catalog.toml').write_text(cat)
    return home,env,[str(binary),'--no-remote','--no-overlay','--catalog',str(home/'catalog.toml')]
def run(argv,env):
    p=subprocess.run(argv,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=35)
    print('EXIT',p.returncode,p.stdout.replace(str(root),'<scratch>').replace(str(repo),'<repo>').strip(),flush=True)
    return p

print('\nCASE: Binary path changes config identity',flush=True)
cat='''[configs.review]
check='{envira} fs write --check "$HOME/file" --content new'
apply='{envira} fs write "$HOME/file" --content new'
revert='{envira} fs restore "$HOME/file"'
'''
home,env,cli=setup('exe-path',cat)
(home/'file').write_text('original')
run(cli+['config','apply','review','-y'],env)
other=home/'envira-other-path'
os.link(binary,other)
run([str(other)]+cli[1:]+['config','revert','review','-y'],env)
print('FILE AFTER REVERT:',(home/'file').read_text())
print('CONFIG STATE:',tomllib.loads((home/'state/state.toml').read_text())['configs']['review'])

print('\nCASE: Crash loses apply ownership',flush=True)
cat='''[configs.review]
check='test -f "$HOME/applied"'
apply='touch "$HOME/applied"; touch "$HOME/ready"; sleep 30'
revert='rm -f "$HOME/applied"'
'''
home,env,cli=setup('crash',cat)
with (home/'interrupted.log').open('w') as log:
    p=subprocess.Popen(cli+['config','apply','review','-y'],env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
    deadline=time.monotonic()+20
    while not (home/'ready').exists() and time.monotonic()<deadline:
        time.sleep(.05)
    assert (home/'ready').exists(), 'Apply fixture did not start'
    os.killpg(p.pid,signal.SIGKILL)
    p.wait()
print('CONFIG RECORD AFTER KILL:',tomllib.loads((home/'state/state.toml').read_text()).get('configs'))
run(cli+['retry','-y'],env)
run(cli+['config','revert','review','-y'],env)
print('APPLIED FILE STILL EXISTS:',(home/'applied').exists())

print('\nCASE: Removed managed route silently falls back',flush=True)
header='''[tools.review]
bin="cat"
detect={cmd='test -f "$HOME/installed" && printf 1.0.0'}
'''
a='''{run='touch "$HOME/installed"; printf A > "$HOME/install-route"',update='printf A > "$HOME/update-route"'}'''
b='''{run='touch "$HOME/installed"; printf B > "$HOME/install-route"',update='printf B > "$HOME/update-route"'}'''
home,env,cli=setup('route-removed',header+'install=['+a+','+b+']\n')
run(cli+['install','review','-y'],env)
(home/'catalog.toml').write_text(header+'install=['+b+']\n')
run(cli+['update','review','--offline','-y'],env)
print('INSTALL/UPDATE ROUTES:',(home/'install-route').read_text(),(home/'update-route').read_text())

print('\nCASE: Existing unrelated Go directory still overrides external Go',flush=True)
go=(repo/'catalog/toolchains.toml').read_text()
cat='[tools.curl]\nmanual="Provided by the test host"\n'+go[go.index('[tools.go]'):go.index('[tools.fnm]')]
home,env,cli=setup('go-directory',cat)
(home/'.go').mkdir()
run(['go','version'],env)
run(cli+['env'],env)
run(['bash','-c','. "$ENVIRA_CONFIG_DIR/env.sh"; go env GOROOT; go version'],env)

print('\nCASE: Multiple managed PATH directories reverse detection order',flush=True)
cat='''[tools.a]
bin="review-bin"
detect={cmd="review-bin"}
install=[{run="exit 99"}]
shell={path=["~/.a/bin"]}
[tools.b]
bin="review-bin"
detect={cmd="review-bin"}
install=[{run="exit 99"}]
shell={path=["~/.b/bin"]}
'''
home,env,cli=setup('path-order',cat)
for name in ['.a','.b']:
    d=home/name/'bin'
    d.mkdir(parents=True)
    p=d/'review-bin'
    p.write_text('#!/bin/sh\nprintf "1.0.0\\n"\n')
    p.chmod(0o755)
p=subprocess.run(cli+['status','--json'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=30)
print('DETECTED PATH:',json.loads(p.stdout)[0]['path'].replace(str(root),'<scratch>'))
run(cli+['env'],env)
run(['bash','-c','. "$ENVIRA_CONFIG_DIR/env.sh"; command -v review-bin'],env)
print('Scratch artifacts:',root,flush=True)
```

### Package semantics, missing-tool recovery and Go standard library

```python
"""Synthetic package and retry fixtures; no real package-manager actions."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

repo=Path.cwd()
root=Path(tempfile.mkdtemp(prefix='envira-fix-extra-'))
base={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_')) and k not in ('GOROOT','GOPATH')}
base.update(ENVIRA_NO_UPDATE_CHECK='1',ENVIRA_PRIVILEGE='none')
def run(cmd,env):
    p=subprocess.run(cmd,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print('EXIT',p.returncode,p.stdout.replace(str(root),'<scratch>').replace(str(repo),'<repo>').strip())
    return p
for method,current,latest in [('apt','1.2.3-1','1.2.3-2'),('dnf','1.0^git-1','1.0.1-1')]:
    home=root/method
    home.mkdir()
    env=dict(base,HOME=str(home),ENVIRA_STATE_DIR=str(home/'state'),ENVIRA_CONFIG_DIR=str(home/'config'),ENVIRA_CACHE_DIR=str(home/'cache'))
    cat=f'''[tools.review]
bin="cat"
detect={{cmd="printf 1.0.0"}}
install=[{{method="{method}"}}]
[methods.{method}]
scope="user"
install="exit 99"
installed={{cmd="printf '{current}'",regex='^(\\S+)'}}
latest={{cmd="printf '{latest}'",regex='^(\\S+)'}}
'''
    (home/'catalog.toml').write_text(cat)
    cli=[str(repo/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(home/'catalog.toml')]
    p=subprocess.run(cli+['status','--latest','--json'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=30)
    data=json.loads(p.stdout)[0]
    print('PACKAGE CASE:',method,'CURRENT:',data['method_version'],'LATEST:',data['latest'],'LABEL:',data['label'])

home=root/'gone-update'
home.mkdir()
env=dict(base,HOME=str(home),ENVIRA_STATE_DIR=str(home/'state'),ENVIRA_CONFIG_DIR=str(home/'config'),ENVIRA_CACHE_DIR=str(home/'cache'))
(home/'catalog.toml').write_text('''[tools.review]
bin="cat"
detect={cmd='test -f "$HOME/installed" && printf 1.0.0'}
install=[{run='touch "$HOME/installed"',update='rm -f "$HOME/installed"; exit 7'}]
''')
cli=[str(repo/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(home/'catalog.toml')]
print('CASE: Gone update recovery')
run(cli+['install','review','-y'],env)
run(cli+['update','review','--offline','-y'],env)
run(cli+['retry','-y'],env)
print('RESTORED:',(home/'installed').exists())

home=root/'go-directory'
home.mkdir()
(home/'.go').mkdir()
env=dict(base,HOME=str(home),ENVIRA_STATE_DIR=str(home/'state'),ENVIRA_CONFIG_DIR=str(home/'config'),ENVIRA_CACHE_DIR=str(home/'cache'))
go=(repo/'catalog/toolchains.toml').read_text()
(home/'catalog.toml').write_text('[tools.curl]\nmanual="Provided by the test host"\n'+go[go.index('[tools.go]'):go.index('[tools.fnm]')])
cli=[str(repo/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(home/'catalog.toml')]
print('CASE: External Go standard library before/after shell init')
run(['go','list','fmt'],env)
run(cli+['env'],env)
run(['bash','-c','. "$ENVIRA_CONFIG_DIR/env.sh"; go list fmt'],env)
print('Scratch artifacts:',root)
```

### TUI and CLI shell-environment recovery

```python
"""Synthetic shell-generation failure; real TUI/CLI, isolated HOME and pseudo-terminal."""
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

root=Path(tempfile.mkdtemp(prefix='envira-fix-tui-retry-'))
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root),ENVIRA_STATE_DIR=str(root/'state'),ENVIRA_CONFIG_DIR=str(root/'config'),ENVIRA_CACHE_DIR=str(root/'cache'),ENVIRA_NO_UPDATE_CHECK='1',ENVIRA_PRIVILEGE='none',TERM='xterm-256color')
(root/'config/env.sh').mkdir(parents=True)
(root/'catalog.toml').write_text('''[configs.review]
check='test -f "$HOME/applied"'
apply='touch "$HOME/applied"'
revert='rm -f "$HOME/applied"'
''')
cli=[str(Path.cwd()/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(root/'catalog.toml')]
initial=subprocess.run(cli+['config','apply','review','-y'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
print('INITIAL EXIT:',initial.returncode)
(root/'config/env.sh').rename(root/'config/previous-blocker')
master,slave=pty.openpty()
fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,150,0,0))
p=subprocess.Popen(cli,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
os.close(slave)
output=bytearray()
def drain(seconds):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if select.select([master],[],[],min(.05,max(0,end-time.monotonic())))[0]:
            try: output.extend(os.read(master,65536))
            except OSError: break
try:
    drain(2)
    os.write(master,b't')
    drain(1)
    print('TUI SAYS NOTHING TO RETRY:',b'The last run left nothing to retry' in output)
    print('ENV GENERATED BY TUI:',(root/'config/env.sh').is_file())
    os.write(master,b'q')
    drain(.5)
finally:
    if p.poll() is None: p.send_signal(signal.SIGTERM)
    try: p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
    os.close(master)
(root/'screen.bin').write_bytes(output)
r=subprocess.run(cli+['retry','-y'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
print('CLI RETRY EXIT:',r.returncode)
print('ENV GENERATED BY CLI:',(root/'config/env.sh').is_file())
print('Scratch artifacts:',root)
```

### Default-shell positive control (container only)

The existing Ubuntu integration image has a disposable account named `user` with passwordless
sudo. This block changes that account's shell only inside a network-disabled disposable
container. Expected observed shells: `/bin/sh`, `/usr/bin/zsh`, `/bin/sh`, both actions exit 0.

```sh
review_script=$(mktemp /tmp/envira-shell-review-XXXXXX.py)
python3 - "$review_script" <<'PYCODE'
from pathlib import Path
import re
import sys
note = Path('.omo/knowledges/deep-review-fix-verification-2026-11-13.md').read_text()
source = re.search(r'^```python-container\n(.*?)^```', note, re.M | re.S).group(1)
Path(sys.argv[1]).write_text(source)
PYCODE
docker run --rm --network none --user root \
  -v "$PWD:/repo:ro" \
  -v "$PWD/target/debug/envira:/opt/envira-review:ro" \
  -v "$review_script:/review.py:ro" \
  envira-it-ubuntu python3 /review.py
```

The cross-entrypoint negative case is the container script from the round-2 note; run it
against the new binary and inspect final config contents rather than exit status alone.

```python-container
"""Container-only fixture: change and restore the throwaway account's login shell."""
from pathlib import Path
import pwd
import subprocess

repo=Path('/repo')
shell=(repo/'catalog/shell.toml').read_text()
cat=Path('/tmp/default-shell-review.toml')
cat.write_text('[tools.zsh]\nmanual="Provided by the container"\n'+shell[shell.index('[configs.default-shell]'):shell.index('[configs.tmux-colors]')])
subprocess.run(['usermod','--shell','/bin/sh','user'],check=True)
cli=['runuser','-u','user','--','env','HOME='+pwd.getpwnam('user').pw_dir,'ENVIRA_NO_UPDATE_CHECK=1','/opt/envira-review','--no-remote','--no-overlay','--catalog',str(cat)]
print('ORIGINAL SHELL:',pwd.getpwnam('user').pw_shell)
for action in ['apply','revert']:
    p=subprocess.run(cli+['config',action,'default-shell','-y'],text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print(action,'EXIT:',p.returncode,'SHELL:',pwd.getpwnam('user').pw_shell)
    if p.returncode: print(p.stdout)
```
