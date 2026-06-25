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
// finished. Without it, calling SDL_Init(VIDEO) / creating a window from a
// plain main() touches the UIKit video backend with no UIApplication and the
// process is killed at launch before any UI appears. SDL_UIKitRunApp is part
// of libSDL2 itself (not libSDL2main), so it is available with our static SDL
// build even though SDL was built with SDL_MAIN_HANDLED.
#[cfg(target_os = "ios")]
fn main() {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int};

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
        let args: Vec<String> = (0..argc as isize)
            .map(|i| unsafe {
                CStr::from_ptr(*argv.offset(i))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        match touchHLE::main(args.into_iter()) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("touchHLE exited with error: {}", e);
                1
            }
        }
    }

    // Build a NUL-terminated C argv from the process arguments, guaranteeing at
    // least argv[0] (touchHLE::main skips argv[0]). SDL_UIKitRunApp copies these
    // before UIApplicationMain, so `owned` only needs to outlive the call.
    let mut owned: Vec<CString> = std::env::args()
        .filter_map(|a| CString::new(a).ok())
        .collect();
    if owned.is_empty() {
        owned.push(CString::new("touchHLE").unwrap());
    }
    let mut argv: Vec<*mut c_char> = owned.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let argc = owned.len() as c_int;

    // SDL was built with SDL_MAIN_HANDLED, so mark main as ready ourselves to
    // avoid SDL_Init failing with "Application not initialized properly".
    // SDL_UIKitRunApp calls UIApplicationMain, which does not return.
    unsafe {
        SDL_SetMainReady();
        SDL_UIKitRunApp(argc, argv.as_mut_ptr(), touchhle_ios_trampoline);
    }
}
