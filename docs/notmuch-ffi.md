# Notmuch FFI

`notm-notmuch` links to `libnotmuch` and generates Rust bindings for the
installed `notmuch.h` at build time. The build script tries `pkg-config notmuch`
first. If a platform lacks a Notmuch `.pc` file but has the standard header and
library installed, it falls back to `/usr/include/notmuch.h` and `-lnotmuch`.
The generated bindings also select optional iterator-status handling by API
presence. With Notmuch 0.40 or newer, iteration distinguishes normal exhaustion
from a runtime invalidation or allocation failure. Older supported headers,
including 0.38, retain their validity-only iterator behavior instead of failing
to compile.

Safe wrappers copy C strings before libnotmuch owners are destroyed and wrap
database, query, message, tag, filename, config, revision, indexing, and tag
mutation lifetimes with Rust RAII types.
