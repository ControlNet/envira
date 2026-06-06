# Rolling distro CI pacman rule

The legacy integration tests include Arch and Manjaro containers. These are rolling-release images, so avoid `pacman -Sy` in Dockerfiles and installer scripts.

Use `pacman -Syu --noconfirm --needed ...` for the first package install in a fresh container. Use `pacman -S --noconfirm --needed ...` for later installs after the full upgrade has already happened.

Reason: `pacman -Sy` can create partial upgrades. In CI this caused Manjaro `curl`, `git`, and `wget` to fail with dynamic library errors such as:

- `curl: symbol lookup error: /usr/lib/libcurl.so.4: undefined symbol: ngtcp2_crypto_get_path_challenge_data2_cb`
- `wget: error while loading shared libraries: libhogweed.so.6`

Manjaro also no longer provides `neofetch` in the current package set; keep it out of required pacman install lists. `verify.sh` treats `neofetch` as optional/warn-only.

The current `manjarolinux/base:latest` image has a generic `/etc/issue` (`\S{PRETTY_NAME} \r (\l)`) and identifies itself through `/etc/os-release` with `ID=manjaro`. Installer distro detection should use `/etc/os-release` for Manjaro; otherwise `run.sh` can fall through to the Arch branch and run the old `pacman -Sy` path.
