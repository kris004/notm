use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

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
        .fold(bindings, |builder, arg| builder.clang_arg(arg));

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set"));
    bindings
        .generate()
        .expect("generate notmuch bindings")
        .write_to_file(out_path.join("bindings.rs"))
        .expect("write notmuch bindings");
}
