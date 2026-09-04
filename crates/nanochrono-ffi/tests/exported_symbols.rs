// SPDX-License-Identifier: Apache-2.0
//! The shared library's exported surface, loaded the way a C program loads it.
//!
//! `abi.rs` links this crate as an `rlib`, which proves the functions behave
//! but says nothing about whether they survive into `libnanochrono.so` under
//! unmangled C names. A `#[no_mangle]` typo, a missing `extern "C"`, or a
//! renamed entry point would all pass there and break every wrapper.
//!
//! The expectations here are read from the wrapper sources rather than typed
//! out, because a hand-written list is a list of what its author remembered.
//! The wrappers declare exactly what they will look up at run time, so making
//! them the source of truth means this test fails the moment the library and
//! its bindings disagree — which is the failure that actually reaches users.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Locates the cdylib next to this test binary.
///
/// Integration-test binaries live in `target/<profile>/deps/`, and cargo puts
/// the cdylib one directory up. Cargo does not hand the path to a test, so it
/// is derived rather than configured.
fn cdylib_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let deps = exe.parent()?.to_path_buf();
    let profile_dir = deps.parent()?.to_path_buf();
    let name = if cfg!(target_os = "windows") {
        "nanochrono.dll"
    } else if cfg!(target_os = "macos") {
        "libnanochrono.dylib"
    } else {
        "libnanochrono.so"
    };
    [profile_dir, deps]
        .iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Every `nc_*` identifier the shipped wrappers name.
///
/// Struct tags and typedefs come out too; they are filtered against what the
/// library exports rather than parsed out, since a `_t` suffix is a
/// convention and not a guarantee.
fn symbols_wrappers_expect() -> BTreeSet<String> {
    let root = repo_root();
    let mut found = BTreeSet::new();
    for dir in ["wrappers", "python"] {
        collect_identifiers(&root.join(dir), &mut found);
    }
    found
}

/// Walks a wrapper tree, pulling `nc_*` identifiers out of every source file.
///
/// Walking beats naming files: a wrapper added later is covered without
/// anyone remembering to add it here.
fn collect_identifiers(dir: &Path, found: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_identifiers(&path, found);
            continue;
        }
        // Binary and build artefacts have no declarations to read.
        let interesting = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("lua" | "js" | "ts" | "go" | "zig" | "py" | "cs" | "java" | "h" | "c" | "rs")
        );
        if !interesting {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if token.starts_with("nc_") && token.len() > 3 {
                found.insert(token.to_string());
            }
        }
    }
}

/// Identifiers that are types, not entry points, and so are never exported.
fn is_a_type_name(name: &str) -> bool {
    name.ends_with("_t") || name == "nc_ctx" || name == "nc_sample_stats"
}

#[test]
fn every_symbol_the_wrappers_declare_is_exported() {
    let Some(path) = cdylib_path() else {
        eprintln!("cdylib not found next to the test binary; skipping");
        return;
    };
    let expected = symbols_wrappers_expect();
    assert!(
        expected.len() > 20,
        "only {} symbols parsed from the wrappers; the sources moved",
        expected.len()
    );

    // SAFETY: loading our own freshly built library, which runs no
    // constructors beyond the Rust runtime's.
    let lib = unsafe { libloading::Library::new(&path) }
        .unwrap_or_else(|e| panic!("could not open {}: {e}", path.display()));

    let missing: Vec<&String> = expected
        .iter()
        .filter(|name| !is_a_type_name(name))
        .filter(|name| {
            let symbol = format!("{name}\0");
            // SAFETY: resolving by name only; nothing is called through the
            // resulting pointer here.
            unsafe { lib.get::<*const ()>(symbol.as_bytes()) }.is_err()
        })
        .collect();

    assert!(
        missing.is_empty(),
        "{} symbol(s) the wrappers call are missing from {}: {missing:?}",
        missing.len(),
        path.display()
    );
}

/// Resolving a symbol proves it exists; calling it proves the ABI works.
#[test]
fn the_library_is_usable_through_its_exported_symbols() {
    let Some(path) = cdylib_path() else {
        eprintln!("cdylib not found next to the test binary; skipping");
        return;
    };
    // SAFETY: as above.
    let lib = unsafe { libloading::Library::new(&path) }.expect("open cdylib");

    type Create = unsafe extern "C" fn() -> *mut std::ffi::c_void;
    type Destroy = unsafe extern "C" fn(*mut std::ffi::c_void);
    type ElapsedNs = unsafe extern "C" fn(*mut std::ffi::c_void) -> u64;
    type InjectFlip = unsafe extern "C" fn(*mut std::ffi::c_void, u32);
    type Verify = unsafe extern "C" fn(*mut std::ffi::c_void) -> u32;
    type CyclesToNs = unsafe extern "C" fn(*mut std::ffi::c_void, u64) -> u64;

    // SAFETY: each signature matches the declaration in the crate root, which
    // is the same contract the generated header states.
    unsafe {
        let create = lib.get::<Create>(b"nc_create\0").expect("nc_create");
        let destroy = lib.get::<Destroy>(b"nc_destroy\0").expect("nc_destroy");
        let elapsed_ns = lib.get::<ElapsedNs>(b"nc_elapsed_ns\0").expect("elapsed");
        let cycles_to_ns = lib
            .get::<CyclesToNs>(b"nc_cycles_to_ns\0")
            .expect("cycles_to_ns");
        let inject = lib
            .get::<InjectFlip>(b"nc_inject_calibration_flip\0")
            .expect("inject");
        let verify = lib
            .get::<Verify>(b"nc_verify_calibration\0")
            .expect("verify");

        let ctx = create();
        assert!(!ctx.is_null(), "nc_create returned null through dlopen");

        let first = elapsed_ns(ctx);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(elapsed_ns(ctx) > first, "elapsed did not advance");

        // The repair path, end to end through the real ABI boundary.
        let truth = cycles_to_ns(ctx, 1_000_000);
        assert!(truth > 0);
        inject(ctx, 29);
        assert_eq!(verify(ctx), 1, "a single flip was not repaired by ECC");
        assert_eq!(
            cycles_to_ns(ctx, 1_000_000),
            truth,
            "conversion changed after a repaired flip"
        );

        destroy(ctx);
    }
}
