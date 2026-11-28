# Deep review, round 3 (2026-11-13)

**Status (2026-11-28):** every finding below was fixed in `573f824`, and the gaps the fix verification found (F1-F7) in `dca939d`; both shipped in 0.1.6. See `deep-review-fix-verification-2026-11-13.md`.

Reviewed commit: `491f7e3`, envira 0.1.5. This pass adds five reproduced findings to
R1-R18 from the preceding two review notes. Production code, catalog entries, project rules,
real configuration and installed tools were not modified.

## Evidence and limits

Two standalone Python 3.11+ standard-library scripts exercised the actual debug binary.
All custom catalogs, files, package-command responses and failure conditions are explicitly
synthetic test fixtures in fresh temporary HOME/state/config/cache directories. No real
package-manager command, elevated operation or vendor download is performed. The APT case
uses the actual bundled APT method with synthetic metadata commands; a failing sudo fixture
prevents accidental escalation. It verifies orchestration, not a vendor repository's contents.

The previous baseline (269 Rust tests, 11 Python tests, Clippy, formatting, shell syntax and
catalog validation) passed and was not repeated against unchanged production code. This pass
examined state/run locks, progress tracking, config composition, route selection, package
metadata freshness and rollback result reporting. No additional defect is claimed for areas
where inspection did not establish one. Native macOS, full distro installations, actual
network repositories, disk exhaustion and power-loss injection remain outside tested scope.

## Confirmed findings

### R19 — P2: managed tools can switch update routes after catalog reordering

Locations: `src/state.rs:89`, `src/plan.rs:357`, `src/status.rs:455`.

Tool state persists method/scope but not the install-entry identity. `route_for` picks the
first currently usable entry with the recorded method. All raw entries share `run`. The
fixture installed through raw route A, reversed A/B in its temporary catalog, and updated
offline. Both commands exited 0; the recorded markers showed `INSTALLED BY: A` and
`UPDATED BY: B`. Thus a catalog reorder can change the updater without changing tool identity.
This promotes the first review's route-identity design follow-up to a reproduced defect;
it is distinct from R8's incorrect adoption of external installations. The current bundled
catalog has no duplicate method names within a tool; custom catalogs and future catalog
evolution expose this defect.

Fix direction: persist stable route identity, install location and relevant parameters;
resolve the same route on update, and require an explicit migration when it is no longer
available. A mutable array index alone is not a durable identity.

### R20 — P2: restoring a key deletes a backup still needed by a whole-file edit

Location: `src/fs.rs:723`.

Backups are shared per file. Whole-file modifications do not occupy an entry in `meta.keys`.
Restoring the final key removes the entire backup slot, including the original whole-file
backup. Reproduction: an original INI file is changed with `fs write`, then changed with
`fs ini-set`; restoring the key succeeds and leaves the file-level change; restoring the
whole file fails with `no envira backup`. There are zero backup metadata slots left.
This is a normal sequence of successful edits, unlike R12's corrupt-metadata trigger.

Fix direction: track outstanding whole-file and key-level effects separately, including
ownership by config/application. Do not infer that an empty key map means no operation still
needs the backup. Define how overlapping whole-file and key edits compose and revert.

### R21 — P2: revert retry mistakes display labels for stable step identities

Locations: `src/plan.rs:482`, `src/plan.rs:1021`.

An action label includes the action name and file, but not its key or command. Different line
edits on one file therefore have identical labels. `resume_revert` compares only label prefixes
to decide what to skip. The fixture successfully reverted its second setting, then deliberately
failed on the first. After enabling that step and reversing the two catalog steps, retry
skipped the still-needed first setting and retried the already-restored second setting.
It failed with `no envira record for key ^second=`; the first setting remained changed.

Fix direction: persist stable step identities and the original action/parameters or a plan
fingerprint. If the current revert plan differs, do not silently reinterpret an old progress
prefix. Retry should use the recorded effect identity or explicitly reject unsafe migration.

### R22 — P2: a config without a reverse command is recorded as successfully reverted

Locations: `src/exec.rs:710`, `src/exec.rs:785`, `src/exec.rs:1027`.

When a revert step has no command, the executor prints a manual-action message, records it as
done, and continues. Revert has no final verification; it returns success and clears `applied`.
A synthetic config that creates a marker but defines no revert reproduced exit 0,
`1/1 succeeded`, state `applied=false` and `last_result=ok`, while its marker still existed.
The bundled catalog contains such configs, including `zellij-config` and `oh-my-opencode`;
some other configs have a mix of reversible and manual-only steps.

Fix direction: represent unsupported/partial revert as such in state, exit status and retry
records. Do not clear applied state or mark manual work completed merely because no command
was available. This is separate from R2's execution of inverses for unowned resources.

### R23 — P2: update refresh can remain permanently behind stale APT indexes

Locations: `src/commands.rs:632`, `src/commands.rs:663`, `catalog/templates.toml:65`,
`catalog/templates.toml:67`.

Latest-version discovery calls `apt-cache policy`, then filters out current tools before an
update plan exists. `apt-refresh` only runs inside the selected update command. When installed
and cached candidate versions match, no update is selected, so this command never refreshes
the metadata that could reveal an update. Even explicit `update <id> --refresh` only bypasses
envira's latest cache; it still queries the old APT indexes.

With the actual bundled APT method and synthetic installed/candidate version 1.0.0, the command
exited 0 and printed `Up to date`; the metadata-refresh fixture was never called. After an
explicit external fixture refresh changed the candidate to 2.0.0, the same command with
`--dry-run` selected an upgrade. Unlike R6, these versions differ in their upstream number;
no revision or epoch parsing ambiguity is involved. Automatic OS metadata refresh can mask
this issue, but envira's update path does not ensure it happens.

Fix direction: check repository-metadata freshness before selecting package updates, subject
to the command's network/privilege policy. Offline mode should retain its explicit semantics.
If metadata cannot be refreshed, report a freshness limitation instead of an unconditional
claim that everything is current. Avoid refreshing once per individual package.

## Replay

Run at repository root on Linux with Python 3.11+ standard library. No package installation is
needed. These are diagnostic reproductions of current bugs, not passing regression tests.
Scratch data is left for inspection. The first script prints the four failure outcomes above;
the second prints an initial APT `Up to date` with no refresh, followed by a selected upgrade
after its external metadata fixture is invoked.

```sh
cargo build --locked
python3 - <<'PY'
from pathlib import Path
import re
import subprocess
import sys
import tempfile
note = Path('.omo/knowledges/deep-review-2026-11-13-round3.md').read_text()
out = Path(tempfile.mkdtemp(prefix='envira-review-round3-replay-'))
for i, source in enumerate(re.findall(r'^```python\n(.*?)^```', note, re.M | re.S), 1):
    script = out / f'case-{i}.py'
    script.write_text(source)
    subprocess.run([sys.executable, str(script)], check=True)
PY
```

### Route, backup and revert reproductions

```python
"""Synthetic offline review fixtures; all mutations use fresh temporary homes."""
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

repo=Path.cwd()
binary=str(repo/'target/debug/envira')
root=Path(tempfile.mkdtemp(prefix='envira-review-round3-'))
base={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
base.update(ENVIRA_NO_UPDATE_CHECK='1', ENVIRA_PRIVILEGE='none', PATH='/usr/bin:/bin')
def setup(name,cat):
    home=root/name
    home.mkdir()
    env=dict(base,HOME=str(home),ENVIRA_STATE_DIR=str(home/'state'),ENVIRA_CONFIG_DIR=str(home/'config'),ENVIRA_CACHE_DIR=str(home/'cache'))
    (home/'catalog.toml').write_text(cat)
    return home,env,[binary,'--no-remote','--no-overlay','--catalog',str(home/'catalog.toml')]
def run(cmd,env):
    p=subprocess.run(cmd,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=40)
    print('EXIT',p.returncode)
    print(p.stdout.replace(str(root),'<scratch>').replace(str(repo),'<repo>').strip())
    return p

print('\nCASE: Managed raw route changes after catalog reorder')
header='''[tools.review]
bin="cat"
detect={cmd='test -f "$HOME/installed" && printf 1.0.0'}
'''
route_a='''{run='touch "$HOME/installed"; printf A > "$HOME/installed-by"', update='printf A > "$HOME/updated-by"'}'''
route_b='''{run='touch "$HOME/installed"; printf B > "$HOME/installed-by"', update='printf B > "$HOME/updated-by"'}'''
home,env,cli=setup('route-change',header+'install=['+route_a+','+route_b+']\n')
run(cli+['install','review','-y'],env)
(home/'catalog.toml').write_text(header+'install=['+route_b+','+route_a+']\n')
run(cli+['update','review','--offline','-y'],env)
print('INSTALLED BY:',(home/'installed-by').read_text())
print('UPDATED BY:',(home/'updated-by').read_text())

print('\nCASE: Key restore deletes an outstanding whole-file backup')
home,env,cli=setup('shared-backup','')
f=home/'settings.ini'
f.write_text('[section]\nkey = original\n')
run([binary,'fs','write',str(f),'--content','[section]\nkey = file-level-change\n'],env)
run([binary,'fs','ini-set',str(f),'section','key','key-level-change'],env)
run([binary,'fs','restore',str(f),'--key','section.key'],env)
run([binary,'fs','restore',str(f)],env)
print('FINAL FILE:',f.read_text().strip())
print('BACKUP SLOTS:',len(list((home/'state/backups').glob('*/meta.toml'))))

print('\nCASE: Revert retry uses ambiguous step labels after catalog reorder')
action='''[actions.line]
check='{envira} fs line --check {file} --match {match} --line {line}'
apply='{envira} fs line {file} --match {match} --line {line}'
revert='if [ {match} = "^first=" ] && [ ! -f "$HOME/allow-first" ]; then exit 7; fi; {envira} fs restore {file} --key {match}'
[configs.review]
'''
first='''{use="line",file="~/settings",match="^first=",line="first=changed"}'''
second='''{use="line",file="~/settings",match="^second=",line="second=changed"}'''
home,env,cli=setup('revert-order',action+'steps=['+first+','+second+']\n')
(home/'settings').write_text('first=original\nsecond=original\n')
run(cli+['config','apply','review','-y'],env)
run(cli+['config','revert','review','-y'],env)
(home/'allow-first').touch()
(home/'catalog.toml').write_text(action+'steps=['+second+','+first+']\n')
run(cli+['retry','-y'],env)
print('FINAL SETTINGS:',(home/'settings').read_text().strip())
for path in sorted((home/'state/logs').glob('*/run.toml')):
    rec=tomllib.loads(path.read_text())
    print('RUN:',rec['command'],[(u['outcome'],u.get('done_steps',[])) for u in rec['units']])

print('\nCASE: Missing revert reports success and records applied=false')
cat='''[configs.review]
check='test -f "$HOME/applied"'
apply='touch "$HOME/applied"'
'''
home,env,cli=setup('missing-revert',cat)
run(cli+['config','apply','review','-y'],env)
run(cli+['config','revert','review','-y'],env)
state=tomllib.loads((home/'state/state.toml').read_text())
print('ARTIFACT STILL EXISTS:',(home/'applied').exists())
print('STATE APPLIED:',state['configs']['review']['applied'])
print('STATE LAST RESULT:',state['configs']['review']['last_result'])
print('\nScratch artifacts:',root)
```

### APT metadata freshness reproduction

```python
"""Synthetic package metadata and command fixtures; no real apt or sudo operations."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib

repo=Path.cwd()
root=Path(tempfile.mkdtemp(prefix='envira-review-apt-freshness-'))
fixtures=root/'fixtures'
fixtures.mkdir()
scripts={
    'apt-cache':'#!/bin/sh\nprintf "  Candidate: "; cat "$HOME/candidate"\n',
    'dpkg-query':'#!/bin/sh\nprintf "1.0.0\\n"\n',
    'apt-get':'#!/bin/sh\nprintf called >> "$HOME/apt-get-calls"; printf "2.0.0\\n" > "$HOME/candidate"\n',
    'sudo':'#!/bin/sh\nprintf "Unexpected sudo fixture call\\n" >&2; exit 99\n',
}
for name,source in scripts.items():
    p=fixtures/name
    p.write_text(source)
    p.chmod(0o755)
(root/'candidate').write_text('1.0.0\n')
(root/'state').mkdir()
(root/'state/state.toml').write_text('''[tools.review]
method="apt"
scope="system"
installed_at="2026-11-13T00:00:00Z"
last_result="ok"
''')
method=tomllib.loads((repo/'catalog/templates.toml').read_text())['methods']['apt']
text='[env]\npath=['+json.dumps(str(fixtures))+']\n[methods.apt]\n'
for key,value in method.items():
    if isinstance(value,dict):
        continue
    text+=key+' = '+json.dumps(value)+'\n'
for key in ['installed','latest']:
    text+='[methods.apt.'+key+']\n'
    for k,v in method[key].items(): text+=k+' = '+json.dumps(v)+'\n'
text+='''[tools.review]
bin="cat"
detect={cmd="printf 1.0.0"}
install=[{method="apt",package="review-fixture"}]
'''
(root/'catalog.toml').write_text(text)
env={k:v for k,v in os.environ.items() if not k.startswith(('ENVIRA_','SUDO_','XDG_'))}
env.update(HOME=str(root),ENVIRA_STATE_DIR=str(root/'state'),ENVIRA_CONFIG_DIR=str(root/'config'),ENVIRA_CACHE_DIR=str(root/'cache'),ENVIRA_NO_UPDATE_CHECK='1',ENVIRA_PRIVILEGE='sudo',PATH=str(fixtures)+':/usr/bin:/bin')
cli=[str(repo/'target/debug/envira'),'--no-remote','--no-overlay','--catalog',str(root/'catalog.toml')]
def run(args):
    p=subprocess.run(args,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=30)
    print('EXIT',p.returncode,p.stdout.replace(str(root),'<scratch>').replace(str(repo),'<repo>').strip())
print('CASE: Explicit forced refresh still consults stale apt metadata')
run(cli+['update','review','--refresh','-y'])
print('METADATA REFRESH CALLED:',(root/'apt-get-calls').exists())
print('Simulating an external metadata refresh with the fixture command')
run([str(fixtures/'apt-get'),'update'])
run(cli+['update','review','--refresh','--dry-run'])
print('Scratch artifacts:',root)
```
