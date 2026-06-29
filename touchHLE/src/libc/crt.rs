/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Stack-protector runtime support: `__stack_chk_guard` and `__stack_chk_fail`.
//!
//! Code built with `-fstack-protector` (common in later SDKs / C++ apps) reads
//! the global canary `__stack_chk_guard` at function entry, stores it on the
//! stack, and re-checks it at exit, calling `__stack_chk_fail` on mismatch.
//! These symbols normally come from libSystem, which touchHLE doesn't link, so
//! the non-lazy pointer to `__stack_chk_guard` is left NULL. The first
//! stack-protected function to run then does `ldr rN, [guard_ptr]` (NULL) →
//! null-page access and an immediate startup crash (observed in Paper Toss
//! World Tour and JellyCar 3).
//!
//! We bind `__stack_chk_guard` to a stable nonzero canary. iOS randomizes the
//! real value, but correctness only requires that the value read at a
//! function's prologue equals the value read at its epilogue — a fixed
//! constant satisfies that for every call.

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::Environment;

/// Arbitrary nonzero canary value. The high null byte mirrors the convention of
/// a "terminator canary" (stops string-overflow reads), though it isn't
/// required here.
const STACK_CHK_GUARD: u32 = 0xDEAD_BE00;

pub const CONSTANTS: ConstantExports = &[(
    "___stack_chk_guard",
    HostConstant::Custom(|env| {
        env.mem
            .alloc_and_write(STACK_CHK_GUARD)
            .cast()
            .cast_const()
    }),
)];

/// `void __stack_chk_fail(void)` — invoked only if the canary check fails, which
/// indicates genuine guest stack corruption (our canary is otherwise constant).
/// It is `noreturn` in the ABI, so we abort rather than return into the middle
/// of a corrupted epilogue.
fn __stack_chk_fail(_env: &mut Environment) {
    panic!("__stack_chk_fail: stack-smashing canary mismatch (guest stack corruption)");
}

pub const FUNCTIONS: FunctionExports = &[export_c_func!(__stack_chk_fail())];
