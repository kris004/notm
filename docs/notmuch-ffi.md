# Notmuch FFI

The build script probes `pkg-config notmuch` first. On this workstation the `.pc` file is missing despite installed headers/library, so the build falls back to `/usr/include/notmuch.h` and `-lnotmuch`. Safe wrappers copy C strings before libnotmuch owners are destroyed and wrap database/query/message lifetimes with Rust RAII.
