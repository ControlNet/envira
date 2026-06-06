# CI Manjaro Action Fix Plan

## Intent
Fix the latest failed GitHub Action until the agent/check passes, with no extra scope.

## Latest action failure
- Run: `26876698608` (`Release to PyPI`) on `main`.
- Failed jobs: `lagacy-test / manjaro user test`, `lagacy-test / manjaro sudo test`.
- RED evidence:
  - `wget: error while loading shared libraries: libhogweed.so.6`
  - `curl: symbol lookup error: /usr/lib/libcurl.so.4: undefined symbol: ngtcp2_crypto_get_path_challenge_data2_cb`
  - git HTTPS helper failed through the same libcurl mismatch.
- Root causes:
  - Rolling-release Manjaro partial upgrade from `pacman -Sy` without a full system upgrade.
  - `run.sh` detected Manjaro via `/etc/issue`, but the current Manjaro base image exposes Manjaro in `/etc/os-release` (`ID=manjaro`) while `/etc/issue` is generic. The sudo job therefore fell through to the Arch branch and reintroduced `pacman -Sy`.

## Scenario contract
1. Happy path: Manjaro user Docker image builds and `bash run_user.sh && bash /workspace/verify.sh` exits 0. Surface: Docker build + Docker run. Test id: `lagacy-test / manjaro user test`.
2. Adjacent sudo path: Manjaro sudo Docker image builds and `bash run.sh && bash /workspace/verify.sh` exits 0. Surface: Docker build + Docker run. Test id: `lagacy-test / manjaro sudo test`.
3. Regression: repo unit suite still passes. Surface: `pixi run test`. Test id: workflow `test / test`.
4. Build regression: package and executable workflow jobs still pass. Surface: `pixi run build-package`, `pixi run build-prod`, `./dist/envira --help`.

## Implemented fix under review
- Rolling distro package installs use full upgrade semantics: `pacman -Syu --noconfirm --needed ...`.
- Later package installs after full upgrade use `pacman -S --noconfirm --needed ...`.
- Manjaro sudo detection now reads `/etc/os-release` with `^ID=manjaro$`, matching the current container surface and keeping the script out of the Arch branch.
- Scope is narrowed to the failing Manjaro path only after reviewer feedback. The current diff changes only `.github/integration-test/Dockerfile-manjaro-user` and the Manjaro branch in `run.sh`; Ubuntu, Arch, Fedora, and workflow matrix behavior are unchanged.

## Evidence captured
- Manjaro user Docker build: passed.
- Manjaro user Docker run/verify: ended `All checks passed`.
- Manjaro sudo Docker build: passed.
- Manjaro sudo Docker run/verify: ended `All checks passed`.
- `pixi run test`: 26 tests, OK.
- `pixi run build-package`: sdist/wheel built and `twine check` PASSED.
- `pixi run build-prod`: built `dist/envira`.
- `./dist/envira --help`: printed CLI usage and exited 0.
- LSP diagnostics: no diagnostics for `run.sh`; Dockerfiles and Markdown have no configured LSP server.
- QA Docker images removed: `envira-test-manjaro-user`, `envira-test-manjaro-sudo`.

## Known non-blocking warnings
- Pixi manifest deprecation warnings for `[tool.pixi.project]` and `[tool.pixi.build-dependencies]` are pre-existing and do not fail CI.
- Setuptools license deprecation warnings are pre-existing and do not fail `twine check`.
- Docker JSONArgsRecommended warning is pre-existing and does not fail CI.
