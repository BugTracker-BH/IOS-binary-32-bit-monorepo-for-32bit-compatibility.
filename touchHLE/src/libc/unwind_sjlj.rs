/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Host-side implementation of the GCC **SjLj** (setjmp/longjmp) C++ exception
//! unwinder, plus the `_Unwind_*` accessor protocol the personality routine
//! calls back into.
//!
//! Background: 32-bit ARM iOS apps built with the SjLj exception model emit
//! `_Unwind_SjLj_Register`/`Unregister` at function entry/exit to maintain a
//! per-thread chain of `SjLj_Function_Context` records, and call
//! `_Unwind_SjLj_RaiseException` on `throw`. The real implementation lives in
//! the bundled libgcc, but it does not execute correctly under touchHLE's
//! loader (its internal linking isn't fully supported), so a thrown exception
//! never reaches its handler -> `std::terminate` -> `abort`. We therefore drive
//! the unwind ourselves: walk the chain, invoke the guest personality routine
//! (`__gxx_personality_sj0`, still guest code in libstdc++) to find/install the
//! handler, then `longjmp` into the landing pad.
//!
//! WARNING: this is ABI-sensitive and was written without the ability to
//! compile or run it. In particular the `__builtin_setjmp` jump-buffer field
//! offsets ([JBUF_FP]/[JBUF_PC]/[JBUF_SP]) are an assumption — they are logged
//! at install time so they can be corrected from device logs.

use crate::abi::{self, CallFromHost, GuestFunction};
use crate::cpu::Cpu;
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{ConstPtr, ConstVoidPtr, MutVoidPtr, Ptr};
use crate::{Environment, ThreadId};
use std::collections::HashMap;

// `struct SjLj_Function_Context` field offsets (GCC unwind-sjlj.c).
const FC_PREV: u32 = 0;
const FC_CALL_SITE: u32 = 4;
const FC_DATA: u32 = 8; // data[0..4] at 8,12,16,20
const FC_PERSONALITY: u32 = 24;
const FC_LSDA: u32 = 28;
const FC_JBUF: u32 = 32; // __builtin_setjmp buffer (void*[5])

// ASSUMED `__builtin_setjmp` buffer layout for ARM (logged for verification).
const JBUF_FP: u32 = FC_JBUF; // [0] frame pointer
const JBUF_PC: u32 = FC_JBUF + 4; // [1] resume address (landing pad)
const JBUF_SP: u32 = FC_JBUF + 8; // [2] stack pointer

// _Unwind_Action
const _UA_SEARCH_PHASE: u32 = 1;
const _UA_CLEANUP_PHASE: u32 = 2;
const _UA_HANDLER_FRAME: u32 = 4;

// _Unwind_Reason_Code
const _URC_FATAL_PHASE2_ERROR: i32 = 2;
const _URC_FATAL_PHASE1_ERROR: i32 = 3;
const _URC_END_OF_STACK: i32 = 5;
const _URC_HANDLER_FOUND: i32 = 6;
const _URC_INSTALL_CONTEXT: i32 = 7;
const _URC_CONTINUE_UNWIND: i32 = 8;

/// Per-guest-thread head of the SjLj context chain.
#[derive(Default)]
pub struct State {
    heads: HashMap<ThreadId, u32>,
}

fn head(env: &mut Environment) -> u32 {
    let t = env.current_thread;
    env.libc_state
        .unwind_sjlj
        .heads
        .get(&t)
        .copied()
        .unwrap_or(0)
}
fn set_head(env: &mut Environment, fc: u32) {
    let t = env.current_thread;
    env.libc_state.unwind_sjlj.heads.insert(t, fc);
}

fn r32(env: &Environment, addr: u32) -> u32 {
    env.mem.read(Ptr::<u32, false>::from_bits(addr))
}
fn w32(env: &mut Environment, addr: u32, val: u32) {
    env.mem.write(Ptr::<u32, true>::from_bits(addr), val);
}

// ---- chain maintenance ------------------------------------------------------

fn _Unwind_SjLj_Register(env: &mut Environment, fc: MutVoidPtr) {
    // Log only the first call: this fires at the entry of every EH function, so
    // logging each would flood the log. Its appearance confirms the host SjLj
    // unwinder is actually linked and active (distinguishing "not in build" from
    // "throw path never reaches RaiseException").
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log!("[eh-sjlj] _Unwind_SjLj_Register first call — host SjLj unwinder is active");
    }
    let fc = fc.to_bits();
    let prev = head(env);
    w32(env, fc + FC_PREV, prev); // fc->prev = head
    set_head(env, fc); // head = fc
}

fn _Unwind_SjLj_Unregister(env: &mut Environment, fc: MutVoidPtr) {
    let fc = fc.to_bits();
    let prev = r32(env, fc + FC_PREV);
    set_head(env, prev); // head = fc->prev
}

fn _Unwind_SjLj_GetContext(env: &mut Environment) -> MutVoidPtr {
    Ptr::from_bits(head(env))
}
fn _Unwind_SjLj_SetContext(env: &mut Environment, fc: MutVoidPtr) {
    set_head(env, fc.to_bits());
}

// ---- accessors called by the personality routine ---------------------------

fn _Unwind_GetLanguageSpecificData(env: &mut Environment, ctx: MutVoidPtr) -> MutVoidPtr {
    Ptr::from_bits(r32(env, ctx.to_bits() + FC_LSDA))
}
fn _Unwind_GetIP(env: &mut Environment, ctx: MutVoidPtr) -> u32 {
    // GCC SjLj convention (unwind-sjlj.c): GetIP returns call_site + 1.
    r32(env, ctx.to_bits() + FC_CALL_SITE).wrapping_add(1)
}
fn _Unwind_GetIPInfo(env: &mut Environment, ctx: MutVoidPtr, ip_before: MutVoidPtr) -> u32 {
    if !ip_before.is_null() {
        w32(env, ip_before.to_bits(), 1);
    }
    r32(env, ctx.to_bits() + FC_CALL_SITE).wrapping_add(1)
}
fn _Unwind_SetIP(env: &mut Environment, ctx: MutVoidPtr, ip: u32) {
    // GCC SjLj convention: the stored call_site is IP - 1.
    w32(env, ctx.to_bits() + FC_CALL_SITE, ip.wrapping_sub(1));
}
fn _Unwind_GetGR(env: &mut Environment, ctx: MutVoidPtr, index: i32) -> u32 {
    r32(env, ctx.to_bits() + FC_DATA + (index as u32) * 4)
}
fn _Unwind_SetGR(env: &mut Environment, ctx: MutVoidPtr, index: i32, value: u32) {
    w32(env, ctx.to_bits() + FC_DATA + (index as u32) * 4, value);
}
fn _Unwind_GetRegionStart(_env: &mut Environment, _ctx: MutVoidPtr) -> u32 {
    0 // SjLj has no region start; call-site indices are absolute
}
fn _Unwind_GetCFA(env: &mut Environment, ctx: MutVoidPtr) -> u32 {
    // CFA ~= the stack pointer saved in the setjmp buffer (jbuf[2]).
    r32(env, ctx.to_bits() + JBUF_SP)
}
fn _Unwind_GetDataRelBase(env: &mut Environment, ctx: MutVoidPtr) -> u32 {
    // For SjLj the "data rel base" is data[0] (set up by the caller).
    r32(env, ctx.to_bits() + FC_DATA)
}
fn _Unwind_GetTextRelBase(env: &mut Environment, ctx: MutVoidPtr) -> u32 {
    r32(env, ctx.to_bits() + FC_DATA + 4)
}

// ---- exception object helpers ----------------------------------------------

fn _Unwind_DeleteException(env: &mut Environment, exc: MutVoidPtr) {
    // exc->exception_cleanup is at offset 8 (after the 8-byte exception_class).
    let cleanup = r32(env, exc.to_bits() + 8);
    if cleanup != 0 {
        let f = GuestFunction::from_addr_with_thumb_bit(cleanup);
        // void cleanup(_Unwind_Reason_Code reason=1 (_URC_FOREIGN_EXCEPTION_CAUGHT), exc)
        let _: () = f.call_from_host(env, (1u32, exc));
    }
}

// ---- the unwinder ------------------------------------------------------------

/// Invoke a guest personality routine:
/// `_Unwind_Reason_Code (*)(int version, _Unwind_Action, uint64 exceptionClass,
///                          _Unwind_Exception*, _Unwind_Context*)`.
/// The 64-bit `exceptionClass` is passed as two u32s; combined with the leading
/// two int args they land in r2/r3 (naturally even-aligned), matching AAPCS.
fn call_personality(
    env: &mut Environment,
    personality: u32,
    actions: u32,
    exc: u32,
    ctx: u32,
) -> i32 {
    let ecl = r32(env, exc); // exception_class low
    let ech = r32(env, exc + 4); // exception_class high
    let f = GuestFunction::from_addr_with_thumb_bit(personality);
    let exc_p: ConstVoidPtr = Ptr::from_bits(exc);
    let ctx_p: ConstVoidPtr = Ptr::from_bits(ctx);
    f.call_from_host(env, (1u32, actions, ecl, ech, exc_p, ctx_p))
}

/// Restore the handler frame's saved context and resume in its landing pad.
/// Mirrors `__builtin_longjmp(fc->jbuf, 1)`: we set the guest LR to the resume
/// address (the SVC return path branches to LR) and restore SP/FP.
fn install_and_longjmp(env: &mut Environment, fc: u32) {
    let new_fp = r32(env, JBUF_FP + (fc + 0));
    let new_pc = r32(env, JBUF_PC + (fc + 0));
    let new_sp = r32(env, JBUF_SP + (fc + 0));
    log!(
        "[eh-sjlj] install handler fc={:#x}: resume pc={:#x} sp={:#x} fp={:#x} (jbuf layout is an assumption)",
        fc,
        new_pc,
        new_sp,
        new_fp
    );
    let regs = env.cpu.regs_mut();
    regs[0] = 1; // longjmp value
    regs[abi::FRAME_POINTER] = new_fp;
    regs[Cpu::SP] = new_sp;
    regs[Cpu::LR] = new_pc; // SVC return path branches to LR
    env.cpu
        .branch(GuestFunction::from_addr_with_thumb_bit(new_pc));
}

fn _Unwind_SjLj_RaiseException(env: &mut Environment, exc: MutVoidPtr) -> i32 {
    let exc = exc.to_bits();
    log!("[eh-sjlj] RaiseException(exc={:#x}) chain head={:#x}", exc, head(env));

    // Phase 1: search for a frame whose personality claims the handler.
    let mut fc = head(env);
    let handler_fc = loop {
        if fc == 0 {
            log!("[eh-sjlj] phase 1: end of chain, no handler -> END_OF_STACK");
            return _URC_END_OF_STACK;
        }
        let personality = r32(env, fc + FC_PERSONALITY);
        if personality != 0 {
            // Diagnostics: a frame inside a live try-region should have a small
            // non-negative call_site index and a non-null LSDA. If call_site is
            // garbage / -1 (0xffffffff) or lsda is 0 for the frames that ought to
            // catch, the unwinder/ABI is wrong. If they look sane yet the
            // personality still says CONTINUE_UNWIND, the app genuinely has no
            // matching catch on this path (the throw is fatal even on-device-
            // equivalent input).
            let call_site = r32(env, fc + FC_CALL_SITE);
            let lsda = r32(env, fc + FC_LSDA);
            let code = call_personality(env, personality, _UA_SEARCH_PHASE, exc, fc);
            log!(
                "[eh-sjlj] phase1 fc={:#x} personality={:#x} call_site={} lsda={:#x} -> {}",
                fc,
                personality,
                call_site as i32,
                lsda,
                code
            );
            if code == _URC_HANDLER_FOUND {
                break fc;
            } else if code != _URC_CONTINUE_UNWIND {
                return _URC_FATAL_PHASE1_ERROR;
            }
        }
        fc = r32(env, fc + FC_PREV);
    };

    // Phase 2: from the top down, run cleanups; install at the handler frame.
    let mut fc = head(env);
    loop {
        let personality = r32(env, fc + FC_PERSONALITY);
        if personality != 0 {
            let mut actions = _UA_CLEANUP_PHASE;
            if fc == handler_fc {
                actions |= _UA_HANDLER_FRAME;
            }
            let code = call_personality(env, personality, actions, exc, fc);
            log!("[eh-sjlj] phase2 fc={:#x} actions={} -> {}", fc, actions, code);
            if code == _URC_INSTALL_CONTEXT {
                _Unwind_SjLj_SetContext(env, Ptr::from_bits(fc));
                install_and_longjmp(env, fc);
                return 0; // control transferred to the landing pad
            } else if code != _URC_CONTINUE_UNWIND {
                return _URC_FATAL_PHASE2_ERROR;
            }
        }
        if fc == handler_fc {
            log!("[eh-sjlj] phase2 reached handler without install -> FATAL");
            return _URC_FATAL_PHASE2_ERROR;
        }
        fc = r32(env, fc + FC_PREV);
    }
}

/// `_Unwind_SjLj_Resume` — re-raise during two-phase cleanup (rethrow path).
fn _Unwind_SjLj_Resume(env: &mut Environment, exc: MutVoidPtr) {
    log!("[eh-sjlj] Resume(exc={:#x})", exc.to_bits());
    let _ = _Unwind_SjLj_RaiseException(env, exc);
    // If it returns, there's no further handler; nothing safe to do.
}

/// `_Unwind_SjLj_Resume_or_Rethrow` — called by libstdc++ when a catch block
/// decides to re-throw (or during forced unwind). Semantically equivalent to
/// RaiseException for our purposes.
fn _Unwind_SjLj_Resume_or_Rethrow(env: &mut Environment, exc: MutVoidPtr) -> i32 {
    log!("[eh-sjlj] Resume_or_Rethrow(exc={:#x})", exc.to_bits());
    _Unwind_SjLj_RaiseException(env, exc)
}

fn _Unwind_SjLj_ForcedUnwind(
    _env: &mut Environment,
    exc: MutVoidPtr,
    _stop: GuestFunction,
    _stop_arg: MutVoidPtr,
) -> i32 {
    log!("[eh-sjlj] ForcedUnwind(exc={:#x}) (unsupported, returning END_OF_STACK)", exc.to_bits());
    _URC_END_OF_STACK
}

/// Host `__cxa_throw`: the C++ throw entry point. libstdc++ provides its own,
/// but at runtime its internal call to `_Unwind_SjLj_RaiseException` does not
/// reach our host implementation (the app registers frames on our chain via our
/// `_Unwind_SjLj_Register`, but the throw never invokes our raise). We shadow
/// `__cxa_throw` so the app's throw routes *directly* into our unwinder, which
/// walks the chain the app already built.
///
/// `__cxa_exception` layout (disassembled from this libstdc++'s `__cxa_throw`):
/// `exceptionType @ obj-0x40`, `exceptionDestructor @ obj-0x3c`, and the
/// `_Unwind_Exception` (unwindHeader) at `obj-0x14`
/// (`exception_class[8], cleanup, private_1, private_2`).
fn __cxa_throw(
    env: &mut Environment,
    thrown_object: MutVoidPtr,
    tinfo: MutVoidPtr,
    dtor: GuestFunction,
) {
    let obj = thrown_object.to_bits();
    log!(
        "[eh-sjlj] __cxa_throw(obj={:#x} tinfo={:#x}) routing to host unwinder",
        obj,
        tinfo.to_bits()
    );
    w32(env, obj - 0x40, tinfo.to_bits()); // exceptionType
    w32(env, obj - 0x3c, dtor.addr_with_thumb_bit()); // exceptionDestructor
    w32(env, obj - 0x14, 0x4355_4e47); // exception_class lo = "GNUC"
    w32(env, obj - 0x10, 0x002b_2b43); // exception_class hi = "C++\0"
    w32(env, obj - 0x0c, 0); // exception_cleanup
    w32(env, obj - 0x08, 0); // private_1
    w32(env, obj - 0x04, 0); // private_2
    let code = _Unwind_SjLj_RaiseException(env, Ptr::from_bits(obj - 0x14));
    if code != 0 {
        // No handler was found anywhere on the SjLj chain.
        //
        // We CANNOT "swallow" this by simply returning: `__cxa_throw` is
        // `[[noreturn]]`, so its caller (e.g. libstdc++'s `__throw_logic_error`)
        // is compiled assuming control never comes back and will fall straight
        // through into `std::terminate`/`abort`. Returning here therefore does
        // not keep the app alive — it just hides the cause. So treat it as the
        // fatal error it is, but report it usefully first.
        //
        // Identify what was thrown via the `std::type_info` (Itanium C++ ABI:
        // vtable @ +0, `const char* __type_name` @ +4 — the mangled name).
        let type_name = if tinfo.to_bits() != 0 {
            let name_ptr = r32(env, tinfo.to_bits() + 4);
            if name_ptr != 0 {
                env.mem
                    .cstr_at_utf8(Ptr::<u8, false>::from_bits(name_ptr))
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|_| "<unreadable>".to_string())
            } else {
                "<null name>".to_string()
            }
        } else {
            "<no type_info (rethrow)>".to_string()
        };
        log!(
            "[eh-sjlj] __cxa_throw: UNCAUGHT C++ exception (type {:?}, raise code={}). \
             No handler on the SjLj chain. Guest stack at the throw site:",
            type_name,
            code
        );
        env.stack_trace_current();
        // A common cause is a touchHLE stub returning NULL/nil that the guest
        // then feeds into `std::string` (-> std::logic_error). The type name
        // and stack above pinpoint which one; fix that stub rather than the
        // throw. Terminate cleanly so touchHLE shows its crash pop-up.
        panic!(
            "Uncaught guest C++ exception of type {:?} (no SjLj handler found). \
             Often a touchHLE stub returned NULL/nil that the app passed to a \
             std::string — see the guest stack above.",
            type_name
        );
    }
}

/// Look up the *real* (guest libstdc++) implementation of a symbol that we have
/// also registered a host shim for. touchHLE links the app's import to our host
/// shim (host functions take precedence over dylib exports), but the dylib's own
/// copy is still present and callable at its exported address. `exported_symbols`
/// values already carry the Thumb bit (`N_ARM_THUMB_DEF`), so the resulting
/// [GuestFunction] decodes in the correct instruction set.
fn resolve_guest_export(env: &Environment, symbol: &str) -> GuestFunction {
    for bin in &env.bins {
        if let Some(&addr) = bin.exported_symbols.get(symbol) {
            return GuestFunction::from_addr_with_thumb_bit(addr);
        }
    }
    panic!(
        "touchHLE NULL-safe std::string shim: could not find real {symbol} \
         in any loaded dylib"
    );
}

/// NULL-tolerant `std::string::string(const char*, const allocator&)`.
///
/// Some apps (notably Backflip Studios titles using Boost) construct a
/// `std::string` from a `const char*` that is NULL under touchHLE — because a
/// stubbed/empty framework call returned nil/NULL where a real device returns a
/// real string. libstdc++'s `_S_construct` reacts to a NULL pointer by throwing
/// `std::logic_error("basic_string::_S_construct null not valid")`. That
/// exception is usually *not* caught by the app (it only catches the exception
/// it actually meant to throw, e.g. `boost::bad_lexical_cast`), so the process
/// aborts. On a real device this never happens because the pointer is never
/// NULL. We restore that behavior: substitute an empty string for NULL, then
/// forward to the real libstdc++ constructor so the object is built with the
/// genuine internal layout.
fn string_from_cstr_nullsafe(
    env: &mut Environment,
    this: MutVoidPtr,
    s: ConstPtr<u8>,
    alloc: ConstVoidPtr,
    symbol: &'static str,
) {
    let s: ConstPtr<u8> = if s.is_null() {
        log!(
            "Note: std::string(const char* = NULL) via {} — substituting \"\" \
             (touchHLE NULL-safety shim) to avoid an uncatchable std::logic_error",
            symbol
        );
        Ptr::from_bits(env.mem.alloc_and_write_cstr(b"").to_bits())
    } else {
        s
    };
    let real = resolve_guest_export(env, symbol);
    // Returns `this` in r0; with R = () we leave r0 untouched after the call,
    // so the constructor's result is preserved for the caller.
    let _: () = real.call_from_host(env, (this, s, alloc));
}

#[allow(non_snake_case)]
fn _ZNSsC1EPKcRKSaIcE(
    env: &mut Environment,
    this: MutVoidPtr,
    s: ConstPtr<u8>,
    alloc: ConstVoidPtr,
) {
    string_from_cstr_nullsafe(env, this, s, alloc, "__ZNSsC1EPKcRKSaIcE")
}

#[allow(non_snake_case)]
fn _ZNSsC2EPKcRKSaIcE(
    env: &mut Environment,
    this: MutVoidPtr,
    s: ConstPtr<u8>,
    alloc: ConstVoidPtr,
) {
    string_from_cstr_nullsafe(env, this, s, alloc, "__ZNSsC2EPKcRKSaIcE")
}

/// Shim for the `std::string(const std::string&)` copy constructor
/// (`_ZNSsC1ERKSs` / `_ZNSsC2ERKSs`). JellyCar 3's
/// `Walaber::Widget_MovingTextBox::drawMe` copies a `std::string` whose internal
/// length is garbage (corrupted from uninitialized/faked state), so libstdc++'s
/// copy ctor → `_M_clone` → `_S_create` throws `std::length_error`, which the
/// app doesn't catch and aborts. If the source's `_Rep` looks corrupt (near-NULL
/// data pointer or absurd length), we build an empty string in `this` instead,
/// matching the `append`/`reserve` clamps above.
fn string_copy_ctor_safe(
    env: &mut Environment,
    this: MutVoidPtr,
    other: MutVoidPtr,
    symbol: &'static str,
) {
    // libstdc++ COW layout: visible object holds the data pointer at offset 0;
    // _Rep::_M_length is at data_ptr - 12.
    let data_ptr: u32 = env.mem.read(Ptr::<u32, false>::from_bits(other.to_bits()));
    let corrupted = if data_ptr < 0x1000 {
        true
    } else {
        let length: u32 = env
            .mem
            .read(Ptr::<u32, false>::from_bits(data_ptr.wrapping_sub(12)));
        length > 256 * 1024 * 1024
    };
    if corrupted {
        // Rate-limit: this fires every frame for JC3's news-ticker widget
        // (Widget_MovingTextBox redraws its corrupted string each frame), which
        // would otherwise flood the log and slow the emulator.
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        if n < 5 || n % 100000 == 0 {
            log!(
                "Note: std::string copy-ctor from corrupted string @ {:?} via {} — \
                 substituting \"\" (touchHLE length_error-safety shim) [x{}]",
                other,
                symbol,
                n + 1
            );
        }
        // Build an empty string in `this` using the real const-char* ctor (the
        // allocator arg is a stateless std::allocator<char>; any readable ptr
        // works).
        let empty_cstr: ConstPtr<u8> =
            Ptr::from_bits(env.mem.alloc_and_write_cstr(b"").to_bits());
        let dummy_alloc: ConstVoidPtr = env.mem.alloc(1).cast_const();
        let real_ctor = resolve_guest_export(env, "__ZNSsC1EPKcRKSaIcE");
        let _: () = real_ctor.call_from_host(env, (this, empty_cstr, dummy_alloc));
        return;
    }
    let real = resolve_guest_export(env, symbol);
    let _: () = real.call_from_host(env, (this, other));
}

#[allow(non_snake_case)]
fn _ZNSsC1ERKSs(env: &mut Environment, this: MutVoidPtr, other: MutVoidPtr) {
    string_copy_ctor_safe(env, this, other, "__ZNSsC1ERKSs")
}

#[allow(non_snake_case)]
fn _ZNSsC2ERKSs(env: &mut Environment, this: MutVoidPtr, other: MutVoidPtr) {
    string_copy_ctor_safe(env, this, other, "__ZNSsC2ERKSs")
}

// TEMPORARY DIAGNOSTIC: intercept the guest's sqlite3 SQL-prep calls, log the SQL
// string, then forward to the real guest libsqlite3 (host functions take link
// precedence over the dylib export, but the dylib's own copy is still callable at
// its exported address — same re-dispatch trick as the std::string shim). This
// reveals the statement that errors with `near "Column"` in JellyCar 3.
#[allow(non_snake_case)]
fn sqlite3_prepare_v2(
    env: &mut Environment,
    db: MutVoidPtr,
    zSql: ConstPtr<u8>,
    nByte: i32,
    ppStmt: MutVoidPtr,
    pzTail: MutVoidPtr,
) -> i32 {
    if !zSql.is_null() {
        let sql = env
            .mem
            .cstr_at_utf8(zSql)
            .map(|s| s.to_owned())
            .unwrap_or_else(|_| String::new());
        // JellyCar 3 uses "Column" as a column name in MainMenuInfo, but
        // touchHLE's bundled SQLite 3.6.10 rejects it as a keyword unless
        // quoted. Rewrite unquoted `Column=` → `"Column"=` so the query parses.
        if sql.contains("Column=") || sql.contains("Column =") {
            let fixed = sql.replace("Column=", "\"Column\"=").replace("Column =", "\"Column\" =");
            log!("[sqlite-fix] rewrote: {:?}", fixed);
            let buf = env.mem.alloc_and_write_cstr(fixed.as_bytes());
            let real = resolve_guest_export(env, "_sqlite3_prepare_v2");
            let result: i32 =
                real.call_from_host(env, (db, buf, -1i32, ppStmt, pzTail));
            env.mem.free(buf.cast());
            return result;
        }
    }
    let real = resolve_guest_export(env, "_sqlite3_prepare_v2");
    real.call_from_host(env, (db, zSql, nByte, ppStmt, pzTail))
}

#[allow(non_snake_case)]
fn sqlite3_prepare(
    env: &mut Environment,
    db: MutVoidPtr,
    zSql: ConstPtr<u8>,
    nByte: i32,
    ppStmt: MutVoidPtr,
    pzTail: MutVoidPtr,
) -> i32 {
    let real = resolve_guest_export(env, "_sqlite3_prepare");
    real.call_from_host(env, (db, zSql, nByte, ppStmt, pzTail))
}

#[allow(non_snake_case)]
fn sqlite3_exec(
    env: &mut Environment,
    db: MutVoidPtr,
    sql: ConstPtr<u8>,
    callback: MutVoidPtr,
    arg: MutVoidPtr,
    errmsg: MutVoidPtr,
) -> i32 {
    let real = resolve_guest_export(env, "_sqlite3_exec");
    real.call_from_host(env, (db, sql, callback, arg, errmsg))
}

/// Shim for `std::string::append(const std::string&)`. JellyCar 3's
/// `SoundManager::playMusic` calls `operator+` which calls `append` with a
/// corrupted `std::string&` (garbage length from uninitialized FMOD state). The
/// real `append` → `reserve` → `_S_create` throws `length_error`. We intercept
/// `append`: if the argument string's internal length field is absurd (> 256MB),
/// we skip the append entirely (the result string stays unchanged), preventing
/// the uncaught exception that kills the app.
#[allow(non_snake_case)]
fn _ZNSs6appendERKSs(env: &mut Environment, this: MutVoidPtr, other: MutVoidPtr) {
    // libstdc++ COW std::string layout: the _Rep is at (data_ptr - 12).
    // At offset 0 of the visible object is the data pointer.
    // _Rep::_M_length is at data_ptr - 12, _M_capacity at data_ptr - 8.
    // We read the data pointer, then read length from _Rep.
    let data_ptr: u32 = env.mem.read(Ptr::<u32, false>::from_bits(other.to_bits()));
    let corrupted = if data_ptr < 0x1000 {
        true // NULL or near-NULL data pointer
    } else {
        let length: u32 = env.mem.read(Ptr::<u32, false>::from_bits(data_ptr.wrapping_sub(12)));
        length > 256 * 1024 * 1024
    };
    if corrupted {
        log!(
            "Note: std::string::append(corrupted string @ {:?}) skipped — \
             likely uninitialized FMOD/sound state (touchHLE append shim)",
            other
        );
        return; // Don't forward — leave `this` unchanged.
    }
    let real = resolve_guest_export(env, "__ZNSs6appendERKSs");
    let _: () = real.call_from_host(env, (this, other));
}

/// Shim for `std::string::reserve(size_t)`. Catches absurd allocation requests
/// from corrupted string state (backup for the append shim above).
#[allow(non_snake_case)]
fn _ZNSs7reserveEm(env: &mut Environment, this: MutVoidPtr, n: u32) {
    if n > 256 * 1024 * 1024 {
        log!(
            "Note: std::string::reserve({:#x}) clamped — likely corrupted \
             string from uninitialized FMOD/sound state (touchHLE reserve shim)",
            n
        );
        return;
    }
    let real = resolve_guest_export(env, "__ZNSs7reserveEm");
    let _: () = real.call_from_host(env, (this, n));
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(__cxa_throw(_, _, _)),
    // NULL-tolerant std::string(const char*, allocator) — see above.
    export_c_func!(_ZNSsC1EPKcRKSaIcE(_, _, _)),
    export_c_func!(_ZNSsC2EPKcRKSaIcE(_, _, _)),
    // length_error-safe std::string copy constructor — see string_copy_ctor_safe.
    export_c_func!(_ZNSsC1ERKSs(_, _)),
    export_c_func!(_ZNSsC2ERKSs(_, _)),
    // TEMP DIAGNOSTIC: log SQL passed to guest sqlite, then forward to the real one.
    export_c_func!(sqlite3_prepare_v2(_, _, _, _, _)),
    export_c_func!(sqlite3_prepare(_, _, _, _, _)),
    export_c_func!(sqlite3_exec(_, _, _, _, _)),
    // Catches absurd std::string::reserve from corrupted FMOD/sound state.
    export_c_func!(_ZNSs7reserveEm(_, _)),
    // Catches corrupted std::string::append (the actual crash path in JC3's playMusic).
    export_c_func!(_ZNSs6appendERKSs(_, _)),
    export_c_func!(_Unwind_SjLj_Register(_)),
    export_c_func!(_Unwind_SjLj_Unregister(_)),
    export_c_func!(_Unwind_SjLj_GetContext()),
    export_c_func!(_Unwind_SjLj_SetContext(_)),
    export_c_func!(_Unwind_SjLj_RaiseException(_)),
    export_c_func!(_Unwind_SjLj_Resume(_)),
    export_c_func!(_Unwind_SjLj_Resume_or_Rethrow(_)),
    export_c_func!(_Unwind_SjLj_ForcedUnwind(_, _, _)),
    export_c_func!(_Unwind_GetLanguageSpecificData(_)),
    export_c_func!(_Unwind_GetIP(_)),
    export_c_func!(_Unwind_GetIPInfo(_, _)),
    export_c_func!(_Unwind_SetIP(_, _)),
    export_c_func!(_Unwind_GetGR(_, _)),
    export_c_func!(_Unwind_SetGR(_, _, _)),
    export_c_func!(_Unwind_GetRegionStart(_)),
    export_c_func!(_Unwind_GetCFA(_)),
    export_c_func!(_Unwind_GetDataRelBase(_)),
    export_c_func!(_Unwind_GetTextRelBase(_)),
    export_c_func!(_Unwind_DeleteException(_)),
];
