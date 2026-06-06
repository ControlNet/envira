# Ubuntu legacy CI matrix

The legacy integration workflow is `.github/workflows/lagacy_test.yaml`. It builds Dockerfiles from `.github/integration-test/Dockerfile-${{ matrix.os }}-${{ matrix.mode }}` and runs either `run.sh` or `run_user.sh` followed by `/workspace/verify.sh` inside the container.

The Ubuntu matrix is intentionally explicit: `ubuntu22.04`, `ubuntu24.04`, and `ubuntu26.04`, each with `sudo` and `user` modes. Each matrix entry must have a matching Dockerfile named `Dockerfile-ubuntu{version}-{mode}` with `FROM ubuntu:{version}`.

The Docker run passes `GITHUB_TOKEN` and `CARGO_HTTP_MULTIPLEXING=false` into the container. This keeps `cargo-binstall` authenticated against GitHub release APIs and avoids Cargo HTTP/2 transport failures during network-heavy installer verification.

Observed local surface command shape:

```bash
GITHUB_TOKEN="$(gh auth token 2>/dev/null || true)" CARGO_HTTP_MULTIPLEXING=false \
  docker run --rm --privileged=true \
  -e CARGO_HTTP_MULTIPLEXING \
  -e GITHUB_TOKEN \
  -e HOME=/home/user \
  -w /home/user \
  -v "$PWD:/workspace" \
  envira-test-ubuntu2404-user \
  bash -lc 'bash "run_user.sh" && bash /workspace/verify.sh'
```

Ubuntu 24.04 and 26.04 user fixtures need `pipx` preinstalled because Ubuntu's externally managed Python environment blocks `python3 -m pip install --user pipx`; without apt `pipx`, `run_user.sh` cannot install `uv` and related pipx tools.
