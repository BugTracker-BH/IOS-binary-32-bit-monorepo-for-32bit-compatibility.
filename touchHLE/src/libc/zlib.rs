/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! zlib's `gz*` file API (`gzopen`, `gzread`, ...).
//!
//! On iOS these are exported by libSystem (which re-exports libz). Games like
//! JellyCar use them to read gzip-compressed asset files. We implement them as
//! host functions: open the guest file via `posix_io`, read it fully, gunzip it
//! with `flate2` if it has a gzip header (otherwise pass the bytes through, to
//! match zlib's transparent handling of uncompressed files), then serve from a
//! host-side cursor.

use crate::dyld::{export_c_func, FunctionExports};
use crate::libc::posix_io::{self, off_t, O_RDONLY};
use crate::mem::{ConstPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::Environment;
use std::collections::HashMap;
use std::io::Read;

const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;

struct GzStream {
    data: Vec<u8>,
    pos: usize,
    /// True if the file was read uncompressed (not gzip): zlib's `gzdirect`.
    direct: bool,
}

#[derive(Default)]
pub struct State {
    streams: HashMap<MutVoidPtr, GzStream>,
}
impl State {
    fn get(env: &mut Environment) -> &mut Self {
        &mut env.libc_state.zlib
    }
}

fn read_whole_guest_file(env: &mut Environment, path: ConstPtr<u8>) -> Option<Vec<u8>> {
    let fd = posix_io::open_direct(env, path, O_RDONLY);
    if fd == -1 {
        return None;
    }
    const CHUNK: GuestUSize = 0x4000;
    let tmp = env.mem.alloc(CHUNK);
    let mut raw: Vec<u8> = Vec::new();
    loop {
        let n = posix_io::read(env, fd, tmp, CHUNK) as i64;
        if n <= 0 {
            break;
        }
        raw.extend_from_slice(env.mem.bytes_at(tmp.cast(), n as GuestUSize));
    }
    env.mem.free(tmp);
    posix_io::close(env, fd);
    Some(raw)
}

fn gzopen(env: &mut Environment, path: ConstPtr<u8>, _mode: ConstPtr<u8>) -> MutVoidPtr {
    let Some(raw) = read_whole_guest_file(env, path) else {
        return Ptr::null();
    };
    let is_gzip = raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b;
    let (data, direct) = if is_gzip {
        let mut out = Vec::new();
        match flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out) {
            Ok(_) => (out, false),
            Err(_) => (raw, true),
        }
    } else {
        (raw, true)
    };
    let handle = env.mem.alloc(1);
    State::get(env)
        .streams
        .insert(handle, GzStream { data, pos: 0, direct });
    handle
}

fn gzdirect(env: &mut Environment, file: MutVoidPtr) -> i32 {
    match State::get(env).streams.get(&file) {
        Some(s) if s.direct => 1,
        _ => 0,
    }
}

fn gzread(env: &mut Environment, file: MutVoidPtr, buf: MutVoidPtr, len: GuestUSize) -> i32 {
    let chunk = {
        let Some(s) = State::get(env).streams.get_mut(&file) else {
            return -1;
        };
        let remaining = s.data.len() - s.pos;
        let n = (len as usize).min(remaining);
        let end = s.pos + n;
        let chunk = s.data[s.pos..end].to_vec();
        s.pos = end;
        chunk
    };
    if !chunk.is_empty() {
        env.mem
            .bytes_at_mut(buf.cast(), chunk.len() as GuestUSize)
            .copy_from_slice(&chunk);
    }
    chunk.len() as i32
}

fn gzgets(env: &mut Environment, file: MutVoidPtr, buf: MutPtr<u8>, len: i32) -> MutPtr<u8> {
    if len <= 0 {
        return Ptr::null();
    }
    let max = (len - 1) as usize;
    let line = {
        let Some(s) = State::get(env).streams.get_mut(&file) else {
            return Ptr::null();
        };
        if s.pos >= s.data.len() {
            return Ptr::null();
        }
        let mut v: Vec<u8> = Vec::new();
        while v.len() < max && s.pos < s.data.len() {
            let b = s.data[s.pos];
            s.pos += 1;
            v.push(b);
            if b == b'\n' {
                break;
            }
        }
        v
    };
    let out = env.mem.bytes_at_mut(buf, (line.len() + 1) as GuestUSize);
    out[..line.len()].copy_from_slice(&line);
    out[line.len()] = 0;
    buf
}

fn gzgetc(env: &mut Environment, file: MutVoidPtr) -> i32 {
    let Some(s) = State::get(env).streams.get_mut(&file) else {
        return -1;
    };
    if s.pos >= s.data.len() {
        return -1;
    }
    let b = s.data[s.pos];
    s.pos += 1;
    b as i32
}

fn gzeof(env: &mut Environment, file: MutVoidPtr) -> i32 {
    match State::get(env).streams.get(&file) {
        Some(s) if s.pos < s.data.len() => 0,
        _ => 1,
    }
}

fn gztell(env: &mut Environment, file: MutVoidPtr) -> off_t {
    match State::get(env).streams.get(&file) {
        Some(s) => s.pos as off_t,
        None => -1,
    }
}

fn gzrewind(env: &mut Environment, file: MutVoidPtr) -> i32 {
    match State::get(env).streams.get_mut(&file) {
        Some(s) => {
            s.pos = 0;
            0
        }
        None => -1,
    }
}

fn gzseek(env: &mut Environment, file: MutVoidPtr, offset: off_t, whence: i32) -> off_t {
    let Some(s) = State::get(env).streams.get_mut(&file) else {
        return -1;
    };
    let base = match whence {
        SEEK_SET => 0i64,
        SEEK_CUR => s.pos as i64,
        SEEK_END => s.data.len() as i64,
        _ => return -1,
    };
    let newpos = base + offset as i64;
    if newpos < 0 || newpos as usize > s.data.len() {
        return -1;
    }
    s.pos = newpos as usize;
    s.pos as off_t
}

fn gzclose(env: &mut Environment, file: MutVoidPtr) -> i32 {
    if State::get(env).streams.remove(&file).is_some() {
        env.mem.free(file);
        0
    } else {
        -1
    }
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(gzopen(_, _)),
    export_c_func!(gzdirect(_)),
    export_c_func!(gzread(_, _, _)),
    export_c_func!(gzgets(_, _, _)),
    export_c_func!(gzgetc(_)),
    export_c_func!(gzeof(_)),
    export_c_func!(gztell(_)),
    export_c_func!(gzrewind(_)),
    export_c_func!(gzseek(_, _, _)),
    export_c_func!(gzclose(_)),
];
