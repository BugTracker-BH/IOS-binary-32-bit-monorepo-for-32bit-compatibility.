/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CommonCrypto and friends

use crate::dyld::FunctionExports;
use crate::mem::{ConstVoidPtr, MutPtr, MutVoidPtr};
use crate::{export_c_func, Environment};
use digest::Digest;
use md5::Md5;
use sha1::Sha1;
use sha2::Sha256;
use std::collections::HashMap;

/// State for the streaming CommonCrypto hash APIs (`CC_SHA256_Init/Update/Final`),
/// keyed by the guest `CC_SHA256_CTX *` pointer. The guest's context struct is
/// opaque to us; we keep the real hasher state host-side.
#[derive(Default)]
pub struct State {
    sha256_ctxs: HashMap<u32, Sha256>,
}

fn CC_MD5(env: &mut Environment, data: ConstVoidPtr, len: u32, md: MutPtr<u8>) -> MutPtr<u8> {
    let mut hasher = Md5::new();
    hasher.update(env.mem.bytes_at(data.cast(), len));
    let digest = hasher.finalize();
    env.mem.bytes_at_mut(md, 16).copy_from_slice(&digest[..]);
    md
}

fn CC_SHA1(env: &mut Environment, data: ConstVoidPtr, len: u32, md: MutPtr<u8>) -> MutPtr<u8> {
    let mut hasher = Sha1::new();
    hasher.update(env.mem.bytes_at(data.cast(), len));
    let digest = hasher.finalize();
    env.mem.bytes_at_mut(md, 20).copy_from_slice(&digest[..]);
    md
}

/// `CC_SHA256(data, len, md)` — one-shot SHA-256.
fn CC_SHA256(env: &mut Environment, data: ConstVoidPtr, len: u32, md: MutPtr<u8>) -> MutPtr<u8> {
    let mut hasher = Sha256::new();
    hasher.update(env.mem.bytes_at(data.cast(), len));
    let digest = hasher.finalize();
    env.mem.bytes_at_mut(md, 32).copy_from_slice(&digest[..]);
    md
}

/// `CC_SHA256_Init(CC_SHA256_CTX *c)` — begin a streaming SHA-256.
fn CC_SHA256_Init(env: &mut Environment, c: MutVoidPtr) -> i32 {
    env.libc_state
        .crypto
        .sha256_ctxs
        .insert(c.to_bits(), Sha256::new());
    1 // CommonCrypto returns 1 on success
}

/// `CC_SHA256_Update(CC_SHA256_CTX *c, const void *data, CC_LONG len)`.
fn CC_SHA256_Update(env: &mut Environment, c: MutVoidPtr, data: ConstVoidPtr, len: u32) -> i32 {
    let bytes = env.mem.bytes_at(data.cast(), len).to_vec();
    if let Some(hasher) = env.libc_state.crypto.sha256_ctxs.get_mut(&c.to_bits()) {
        hasher.update(&bytes);
    } else {
        log!("Warning: CC_SHA256_Update on unknown context {:?}", c);
    }
    1
}

/// `CC_SHA256_Final(unsigned char *md, CC_SHA256_CTX *c)` — write the 32-byte digest.
fn CC_SHA256_Final(env: &mut Environment, md: MutPtr<u8>, c: MutVoidPtr) -> i32 {
    if let Some(hasher) = env.libc_state.crypto.sha256_ctxs.remove(&c.to_bits()) {
        let digest = hasher.finalize();
        env.mem.bytes_at_mut(md, 32).copy_from_slice(&digest[..]);
    } else {
        log!("Warning: CC_SHA256_Final on unknown context {:?}", c);
    }
    1
}

// Security.framework Keychain stubs. The app uses these to store/retrieve
// credentials. We return "not found" so the app takes its "no saved data"
// fallback path instead of crashing when it dereferences a null result pointer.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// `OSStatus SecItemCopyMatching(CFDictionaryRef query, CFTypeRef *result)`
fn SecItemCopyMatching(_env: &mut Environment, _query: MutVoidPtr, _result: MutVoidPtr) -> i32 {
    ERR_SEC_ITEM_NOT_FOUND
}
/// `OSStatus SecItemAdd(CFDictionaryRef attributes, CFTypeRef *result)`
fn SecItemAdd(_env: &mut Environment, _attributes: MutVoidPtr, _result: MutVoidPtr) -> i32 {
    0 // errSecSuccess — pretend the add succeeded
}
/// `OSStatus SecItemUpdate(CFDictionaryRef query, CFDictionaryRef attributesToUpdate)`
fn SecItemUpdate(_env: &mut Environment, _query: MutVoidPtr, _attrs: MutVoidPtr) -> i32 {
    0 // errSecSuccess
}
/// `OSStatus SecItemDelete(CFDictionaryRef query)`
fn SecItemDelete(_env: &mut Environment, _query: MutVoidPtr) -> i32 {
    0 // errSecSuccess
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CC_MD5(_, _, _)),
    export_c_func!(CC_SHA1(_, _, _)),
    export_c_func!(CC_SHA256(_, _, _)),
    export_c_func!(CC_SHA256_Init(_)),
    export_c_func!(CC_SHA256_Update(_, _, _)),
    export_c_func!(CC_SHA256_Final(_, _)),
    export_c_func!(SecItemCopyMatching(_, _)),
    export_c_func!(SecItemAdd(_, _)),
    export_c_func!(SecItemUpdate(_, _)),
    export_c_func!(SecItemDelete(_)),
    export_c_func!(CCCrypt(_, _, _, _, _, _, _, _, _, _, _)),
];

/// `CCCrypt` — one-shot CommonCrypto encrypt/decrypt.
///
/// TODO: real cipher (AES/DES). For now this is a PASSTHROUGH stub: it copies the
/// input to the output unchanged and reports `kCCSuccess`. This unblocks apps
/// that round-trip their own data (encrypt then later decrypt → identity both
/// ways preserves the data) or that only encrypt for transport (e.g. an ad SDK).
/// It does NOT correctly decrypt data encrypted elsewhere (e.g. a pre-encrypted
/// asset) — if a game needs that, implement real AES here.
#[allow(clippy::too_many_arguments)]
fn CCCrypt(
    env: &mut Environment,
    op: u32,
    alg: u32,
    options: u32,
    _key: ConstVoidPtr,
    key_length: u32,
    _iv: ConstVoidPtr,
    data_in: ConstVoidPtr,
    data_in_length: u32,
    data_out: MutVoidPtr,
    data_out_available: u32,
    data_out_moved: MutPtr<u32>,
) -> i32 {
    const KCC_SUCCESS: i32 = 0;
    const KCC_BUFFER_TOO_SMALL: i32 = -4301;
    log!(
        "TODO: CCCrypt(op={}, alg={}, options={}, keyLen={}, inLen={}) — passthrough stub (no real crypto)",
        op,
        alg,
        options,
        key_length,
        data_in_length
    );
    if data_out_available < data_in_length {
        return KCC_BUFFER_TOO_SMALL;
    }
    if data_in_length > 0 && !data_in.is_null() && !data_out.is_null() {
        let input = env.mem.bytes_at(data_in.cast(), data_in_length).to_vec();
        env.mem
            .bytes_at_mut(data_out.cast(), data_in_length)
            .copy_from_slice(&input);
    }
    if !data_out_moved.is_null() {
        env.mem.write(data_out_moved, data_in_length);
    }
    KCC_SUCCESS
}
