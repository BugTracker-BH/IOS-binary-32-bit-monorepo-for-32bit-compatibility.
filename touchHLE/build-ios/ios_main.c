/*
 * iOS entry shim for touchHLE.
 *
 * On iOS, libSDL2main provides the real main(), which calls
 * SDL_UIKitRunApp(argc, argv, SDL_main). So we define SDL_main here and forward
 * it into the Rust core via an exported symbol.
 *
 * Rust side (gated on target_os = "ios") must export:
 *     #[no_mangle] pub extern "C" fn touchHLE_ios_main(argc: c_int,
 *                                                       argv: *mut *mut c_char) -> c_int
 * and Cargo.toml [lib] crate-type must include "staticlib".
 */
#include "SDL_main.h"

extern int touchHLE_ios_main(int argc, char *argv[]);

int SDL_main(int argc, char *argv[]) {
    return touchHLE_ios_main(argc, argv);
}
