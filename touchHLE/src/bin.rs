/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
// Allow the crate to have a non-snake-case name (touchHLE).
// This also allows items in the crate to have non-snake-case names.
#![allow(non_snake_case)]

#[cfg(not(target_os = "ios"))]
fn main() -> Result<(), String> {
    touchHLE::main(std::env::args())
}

// On iOS, an SDL2 app must be launched through SDL_UIKitRunApp. That function
// runs UIApplicationMain and installs SDL's UIApplicationDelegate / UIWindow /
// CAEAGLLayer and the iOS run loop, *then* invokes the supplied main function
// (here a trampoline into the Rust core) on the main thread once launch has
// finished. SDL_UIKitRunApp is part of libSDL2 itself (not libSDL2main), so it
// is available with our static SDL build even though SDL was built with
// SDL_MAIN_HANDLED.
#[cfg(target_os = "ios")]
fn main() {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int};

    // A SpringBoard launch has no attached console, so redirect stdout+stderr to
    // a file we can read over SSH afterwards. The app has the no-container
    // entitlement, so /var/mobile is writable. stderr is unbuffered in Rust, so
    // breadcrumbs / panics / the trampoline's error print land immediately even
    // if the process is killed.
    unsafe {
        if let Ok(path) = CString::new("/var/mobile/touchHLE-ios.log") {
            let fd = libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o644,
            );
            if fd >= 0 {
                libc::dup2(fd, 1);
                libc::dup2(fd, 2);
                if fd > 2 {
                    libc::close(fd);
                }
            }
        }
    }
    eprintln!("[ios] main() reached; log redirected; about to call SDL_UIKitRunApp");

    extern "C" {
        fn SDL_SetMainReady();
        fn SDL_UIKitRunApp(
            argc: c_int,
            argv: *mut *mut c_char,
            main_function: extern "C" fn(c_int, *mut *mut c_char) -> c_int,
        ) -> c_int;
    }

    // Called by SDL on the main thread after the iOS app has finished launching.
    extern "C" fn touchhle_ios_trampoline(argc: c_int, argv: *mut *mut c_char) -> c_int {
        eprintln!("[ios] trampoline entered (argc={argc}); calling touchHLE::main");
        let args: Vec<String> = (0..argc as isize)
            .map(|i| unsafe {
                CStr::from_ptr(*argv.offset(i))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let result = touchHLE::main(args.into_iter());
        match result {
            Ok(()) => {
                eprintln!("[ios] touchHLE::main returned Ok");
                0
            }
            Err(e) => {
                eprintln!("[ios] touchHLE::main returned Err: {e}");
                1
            }
        }
    }

    // Build a NUL-terminated C argv, guaranteeing argv[0] (touchHLE::main skips
    // it). SDL_UIKitRunApp copies these before UIApplicationMain, so `owned`
    // only needs to outlive the call.
    let mut owned: Vec<CString> = std::env::args()
        .filter_map(|a| CString::new(a).ok())
        .collect();
    if owned.is_empty() {
        owned.push(CString::new("touchHLE").unwrap());
    }
    let mut argv: Vec<*mut c_char> = owned.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let argc = owned.len() as c_int;

    // SDL was built with SDL_MAIN_HANDLED, so mark main ready ourselves to avoid
    // SDL_Init failing with "Application not initialized properly".
    // SDL_UIKitRunApp calls UIApplicationMain, which does not return.
    unsafe {
        SDL_SetMainReady();
        SDL_UIKitRunApp(argc, argv.as_mut_ptr(), touchhle_ios_trampoline);
    }
    eprintln!("[ios] SDL_UIKitRunApp returned (unexpected)");
}
