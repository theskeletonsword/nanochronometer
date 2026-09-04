use std::env;

fn main() {
    if let Ok(dir) = env::var("NANOCHRONO_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", dir);
    }
    if cfg!(feature = "static") {
        println!("cargo:rustc-link-lib=static=nanochrono_static");
    } else {
        println!("cargo:rustc-link-lib=dylib=nanochrono");
    }
}
