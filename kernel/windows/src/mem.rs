// SPDX-License-Identifier: MIT

//! Self-contained `mem*` implementations.
//!
//! A Windows kernel driver may only import from kernel images (`ntoskrnl.exe`
//! etc.). The prebuilt `core`/`compiler_builtins` for the gnullvm targets
//! references CRT `memcpy`, which the GNU driver would resolve to
//! `api-ms-win-crt-*.dll` — a user-mode DLL that is invalid in kernel mode.
//! Defining the symbols locally keeps every import inside `ntoskrnl.exe`.

// SAFETY: all routines expect caller-validated memory; this module only adds
// bytewise access and never touches undefined memory on its own.
#![allow(clippy::needless_return)]

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0usize;
    while i < n {
        // SAFETY: caller guarantees dst/src valid for n bytes.
        unsafe {
            *dst.add(i) = *src.add(i);
        }
        i += 1;
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if dst as usize <= src as usize {
        let mut i = 0usize;
        while i < n {
            // SAFETY: caller guarantees ranges.
            unsafe {
                *dst.add(i) = *src.add(i);
            }
            i += 1;
        }
    } else {
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: caller guarantees ranges.
            unsafe {
                *dst.add(i) = *src.add(i);
            }
        }
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    let v = c as u8;
    let mut i = 0usize;
    while i < n {
        // SAFETY: caller guarantees range.
        unsafe {
            *s.add(i) = v;
        }
        i += 1;
    }
    s
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    let mut i = 0usize;
    while i < n {
        // SAFETY: caller guarantees ranges.
        let x = unsafe { *a.add(i) };
        let y = unsafe { *b.add(i) };
        if x != y {
            return (x as i32) - (y as i32);
        }
        i += 1;
    }
    0
}