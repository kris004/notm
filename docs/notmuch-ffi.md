# Notmuch FFI

`notm-notmuch` links to `libnotmuch` and generates Rust bindings for the
installed `notmuch.h` at build time. The build script tries `pkg-config notmuch`
first. If a platform lacks a Notmuch `.pc` file but has the standard header and
library installed, it falls back to `/usr/include/notmuch.h` and `-lnotmuch`.

Safe wrappers copy C strings before libnotmuch owners are destroyed and wrap
database, query, message, tag, filename, config, revision, indexing, and tag
mutation lifetimes with Rust RAII types.
