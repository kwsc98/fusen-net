# Local container secrets

Place development certificates, private keys, CA certificates, and generated token files in this
directory. Only this README and `.gitignore` belong in version control.

Never use the development material from this directory in a public or production deployment.

The example Server runs as UID/GID `10001`; its file-backed service key must be owned by that UID
and use mode `0400`. The example Agent runs as root, so its token must be root-owned with mode
`0400`. Many Compose implementations do not remap file-backed secret ownership.
