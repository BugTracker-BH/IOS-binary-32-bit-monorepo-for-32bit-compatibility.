/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Objective-C ARC (Automatic Reference Counting) runtime helpers:
//! `objc_retain`, `objc_release`, `objc_storeStrong`, and the autorelease/weak
//! families.
//!
//! ARC-compiled apps emit calls to these libobjc functions pervasively (every
//! strong assignment, every returned object). touchHLE didn't implement them,
//! so an ARC app crashes ("Call to unimplemented function objc_retain") the
//! moment it touches a retained object. We map them onto touchHLE's existing
//! `retain`/`release`/`autorelease` messaging.
//!
//! Weak references are best-effort and NON-zeroing: touchHLE has no weak
//! registry, so a weak ref is stored as a plain pointer. The only risk is a
//! use-after-free if an object is freed while weakly referenced — acceptable
//! for best-effort compatibility, and far better than a hard crash.

use super::messages::{autorelease, release, retain};
use super::{id, nil};
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::MutPtr;
use crate::Environment;

fn objc_retain(env: &mut Environment, object: id) -> id {
    retain(env, object)
}

fn objc_release(env: &mut Environment, object: id) {
    release(env, object)
}

fn objc_autorelease(env: &mut Environment, object: id) -> id {
    autorelease(env, object)
}

fn objc_retainAutorelease(env: &mut Environment, object: id) -> id {
    let object = retain(env, object);
    autorelease(env, object)
}

/// Producer side of the ARC return-value optimization. The TLS hand-off is just
/// an optimization; a plain autorelease is semantically correct.
fn objc_autoreleaseReturnValue(env: &mut Environment, object: id) -> id {
    autorelease(env, object)
}

fn objc_retainAutoreleaseReturnValue(env: &mut Environment, object: id) -> id {
    let object = retain(env, object);
    autorelease(env, object)
}

/// Consumer side of the optimization: retain the (autoreleased) return value.
fn objc_retainAutoreleasedReturnValue(env: &mut Environment, object: id) -> id {
    retain(env, object)
}

/// `void objc_storeStrong(id *location, id object)`
fn objc_storeStrong(env: &mut Environment, location: MutPtr<id>, object: id) {
    let old: id = env.mem.read(location);
    if old == object {
        return;
    }
    let object = retain(env, object);
    env.mem.write(location, object);
    release(env, old);
}

/// Without real block-copy support, retain the block like an ordinary object.
fn objc_retainBlock(env: &mut Environment, block: id) -> id {
    retain(env, block)
}

fn objc_initWeak(env: &mut Environment, location: MutPtr<id>, object: id) -> id {
    env.mem.write(location, object);
    object
}

fn objc_storeWeak(env: &mut Environment, location: MutPtr<id>, object: id) -> id {
    env.mem.write(location, object);
    object
}

fn objc_loadWeakRetained(env: &mut Environment, location: MutPtr<id>) -> id {
    let object: id = env.mem.read(location);
    retain(env, object)
}

fn objc_loadWeak(env: &mut Environment, location: MutPtr<id>) -> id {
    let object: id = env.mem.read(location);
    let object = retain(env, object);
    autorelease(env, object)
}

fn objc_destroyWeak(env: &mut Environment, location: MutPtr<id>) {
    env.mem.write(location, nil);
}

fn objc_copyWeak(env: &mut Environment, to: MutPtr<id>, from: MutPtr<id>) {
    let object: id = env.mem.read(from);
    env.mem.write(to, object);
}

/// Called when a collection is mutated during fast enumeration. Real libobjc
/// aborts; we log and continue (best-effort).
fn objc_enumerationMutation(_env: &mut Environment, object: id) {
    log!("Warning: objc_enumerationMutation({:?}) ignored", object);
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(objc_retain(_)),
    export_c_func!(objc_release(_)),
    export_c_func!(objc_autorelease(_)),
    export_c_func!(objc_retainAutorelease(_)),
    export_c_func!(objc_autoreleaseReturnValue(_)),
    export_c_func!(objc_retainAutoreleaseReturnValue(_)),
    export_c_func!(objc_retainAutoreleasedReturnValue(_)),
    export_c_func!(objc_storeStrong(_, _)),
    export_c_func!(objc_retainBlock(_)),
    export_c_func!(objc_initWeak(_, _)),
    export_c_func!(objc_storeWeak(_, _)),
    export_c_func!(objc_loadWeakRetained(_)),
    export_c_func!(objc_loadWeak(_)),
    export_c_func!(objc_destroyWeak(_)),
    export_c_func!(objc_copyWeak(_, _)),
    export_c_func!(objc_enumerationMutation(_)),
];
