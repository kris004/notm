use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rustc-check-cfg=cfg(notmuch_has_iterator_status)");

    let mut clang_args = Vec::new();
    match pkg_config::Config::new().probe("notmuch") {
        Ok(lib) => {
            for path in lib.include_paths {
                clang_args.push(format!("-I{}", path.display()));
            }
            println!("cargo:rustc-env=NOTM_NOTMUCH_LINK_MODE=pkg-config");
        }
        Err(err) => {
            eprintln!(
                "notm-notmuch: pkg-config notmuch failed: {err}; falling back to system header/library"
            );
            println!("cargo:rustc-link-lib=notmuch");
            println!("cargo:rustc-link-search=native=/usr/lib64");
            println!("cargo:rustc-env=NOTM_NOTMUCH_LINK_MODE=fallback-system");
            clang_args.push("-I/usr/include".to_string());
        }
    }

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("notmuch_.*")
        .allowlist_type("notmuch_.*")
        .allowlist_var("NOTMUCH_.*")
        .rustified_enum("notmuch_.*")
        .derive_default(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    let bindings = clang_args
        .into_iter()
        .fold(bindings, |builder, arg| builder.clang_arg(arg))
        .generate()
        .expect("generate notmuch bindings");

    // Detect the API from the generated bindings rather than assuming a package
    // version: distributions may backport it, and the fallback path has no .pc
    // metadata. Notmuch 0.40 introduced both status functions and enum values.
    let has_iterator_status = {
        let source = bindings.to_string();
        source.contains("pub fn notmuch_threads_status")
            && source.contains("pub fn notmuch_messages_status")
            && source.contains("NOTMUCH_STATUS_ITERATOR_EXHAUSTED")
            && source.contains("NOTMUCH_STATUS_OPERATION_INVALIDATED")
    };
    if has_iterator_status {
        println!("cargo:rustc-cfg=notmuch_has_iterator_status");
    }

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set"));
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("write notmuch bindings");
}
