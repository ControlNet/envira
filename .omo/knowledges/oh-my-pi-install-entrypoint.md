# Oh My Pi installation entrypoint

As of 2026-09-01, the Oh My Pi README recommends:

```sh
curl -fsSL https://omp.sh/install | sh
```

The short URL redirects to the upstream `can1357/oh-my-pi` installer. Prefer the
short official entrypoint in `run.sh` and `run_user.sh` so upstream script moves
do not require repository changes.
