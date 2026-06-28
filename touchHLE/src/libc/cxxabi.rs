/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `cxxabi.h`
//!
//! Resources:
//! - [Itanium C++ ABI specification](https://itanium-cxx-abi.github.io/cxx-abi/abi.html#dso-dtor-runtime-api)

use crate::abi::GuestFunction;
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::MutVoidPtr;
use crate::Environment;

fn __cxa_atexit(
    _env: &mut Environment,
    func: GuestFunction, // void (*func)(void *)
    p: MutVoidPtr,
    d: MutVoidPtr,
) -> i32 {
    // TODO: when this is implemented, make sure it's properly compatible with
    // C atexit.
    log!(
        "TODO: __cxa_atexit({:?}, {:?}, {:?}) (unimplemented)",
        func,
        p,
        d
    );
    0 // success
}

fn __cxa_finalize(_env: &mut Environment, d: MutVoidPtr) {
    log!("TODO: __cxa_finalize({:?}) (unimplemented)", d);
}

// SjLj (setjmp/longjmp) exception unwinding runtime, used by 32-bit ARM apps
// compiled with the SjLj exception model (the default for armv7 with some
// toolchains, e.g. several older games). These symbols normally come from
// libgcc/libunwind, which we don't link. `_Unwind_SjLj_Register` and
// `_Unwind_SjLj_Unregister` are emitted at the entry/exit of every function
// that can participate in unwinding, so they are called constantly even when
// no exception is ever thrown — that's why a missing stub crashes immediately
// on launch (e.g. Paper Toss World Tour).
//
// The runtime maintains a per-thread singly linked list (LIFO stack) of
// `SjLj_Function_Context` records; `prev` is the first field (offset 0). We
// faithfully maintain that chain so that, if an exception *is* thrown, the
// (currently stubbed) raise path would at least see a well-formed context
// stack. The guest runs on a single coroutine thread, so a thread-local top
// pointer is correct here.
thread_local! {
    static SJLJ_TOP: std::cell::Cell<u32> = std::cell::Cell::new(0);
}

/// `void _Unwind_SjLj_Register(struct SjLj_Function_Context *fc)`
fn _Unwind_SjLj_Register(env: &mut Environment, fc: MutVoidPtr) {
    // fc->prev = top; top = fc;
    let prev_slot = fc.cast::<u32>();
    let top = SJLJ_TOP.with(|c| c.get());
    env.mem.write(prev_slot, top);
    SJLJ_TOP.with(|c| c.set(fc.to_bits()));
}

/// `void _Unwind_SjLj_Unregister(struct SjLj_Function_Context *fc)`
fn _Unwind_SjLj_Unregister(env: &mut Environment, fc: MutVoidPtr) {
    // top = fc->prev;
    let prev_slot = fc.cast::<u32>();
    let prev: u32 = env.mem.read(prev_slot);
    SJLJ_TOP.with(|c| c.set(prev));
}

/// `_Unwind_Reason_Code _Unwind_SjLj_RaiseException(struct _Unwind_Exception *)`
///
/// We cannot truly propagate a C++ exception across the dynarmic CPU boundary,
/// so this is a best-effort stub. Returns `_URC_END_OF_STACK` (5), i.e. "no
/// handler found", which leads the guest runtime to call terminate(). This is
/// only reached if the app actually throws — normal control flow never does.
fn _Unwind_SjLj_RaiseException(_env: &mut Environment, exc: MutVoidPtr) -> i32 {
    log!(
        "TODO: _Unwind_SjLj_RaiseException({:?}) (unimplemented; returning _URC_END_OF_STACK)",
        exc
    );
    5 // _URC_END_OF_STACK
}

/// `void _Unwind_SjLj_Resume(struct _Unwind_Exception *)` (normally noreturn)
fn _Unwind_SjLj_Resume(_env: &mut Environment, exc: MutVoidPtr) {
    log!("TODO: _Unwind_SjLj_Resume({:?}) (unimplemented)", exc);
}

/// `_Unwind_Reason_Code _Unwind_SjLj_ForcedUnwind(exc, stop, stop_arg)`
fn _Unwind_SjLj_ForcedUnwind(
    _env: &mut Environment,
    exc: MutVoidPtr,
    _stop: GuestFunction,
    _stop_arg: MutVoidPtr,
) -> i32 {
    log!(
        "TODO: _Unwind_SjLj_ForcedUnwind({:?}) (unimplemented; returning _URC_END_OF_STACK)",
        exc
    );
    5 // _URC_END_OF_STACK
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(__cxa_atexit(_, _, _)),
    export_c_func!(__cxa_finalize(_)),
    export_c_func!(_Unwind_SjLj_Register(_)),
    export_c_func!(_Unwind_SjLj_Unregister(_)),
    export_c_func!(_Unwind_SjLj_RaiseException(_)),
    export_c_func!(_Unwind_SjLj_Resume(_)),
    export_c_func!(_Unwind_SjLj_ForcedUnwind(_, _, _)),
];
