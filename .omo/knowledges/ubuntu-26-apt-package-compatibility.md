# Ubuntu 26 apt package compatibility

`run.sh` must not install `neofetch` in the Ubuntu apt package list. `ubuntu:26.04` does not provide `neofetch`, so `apt install` fails with `Unable to locate package neofetch`.

The Ubuntu apt install surface was verified in Docker with `ubuntu:24.04` and `ubuntu:26.04` after replacing `neofetch` with packages still available in both images and needed by later installer/verification steps: `software-properties-common`, `pciutils`, and `fontconfig`.
