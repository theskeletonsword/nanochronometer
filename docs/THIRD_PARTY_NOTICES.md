# Third-party notices

This source package can link against OpenSSL and/or libsodium when the matching files are present under `externals/`.

Before distributing a binary release that includes OpenSSL or libsodium DLLs/import libraries, include the official license and notice files from the exact versions you bundled. This repository intentionally does not guess or rewrite those notices because they must match the shipped binaries.

Recommended release layout:

```text
dist\
  licenses\
    openssl-LICENSE.txt
    openssl-NOTICE.txt
    libsodium-LICENSE.txt
```
