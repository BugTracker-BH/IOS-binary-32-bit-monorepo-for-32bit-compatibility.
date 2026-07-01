/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! JellyCar 3 music shim — plays the game's background music through touchHLE's
//! OpenAL stack by hooking `Walaber::SoundManager::playMusic(int track)` at
//! guest address `0x101aac`. JC3-only; installed from the JC3-gated init block
//! in `environment.rs`.

use crate::audio::openal as al;
use crate::audio::openal::al_types::*;
use crate::audio::AudioFile;
use crate::dyld::HostFunction;
use crate::fs::GuestPathBuf;
use crate::mem::{MutPtr, Ptr};
use crate::Environment;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

/// `Walaber::SoundManager::playMusic(int)` (Thumb).
const PLAY_MUSIC_ADDR: u32 = 0x101aac;

/// Standard OpenAL 1.1 enum values not re-exported by openal_soft_wrapper.
const AL_LOOPING: ALenum = 0x1007;
const AL_GAIN: ALenum = 0x100A;

/// Currently playing music source / buffer (0 = none), and track (-1 = none).
static CUR_SOURCE: AtomicU32 = AtomicU32::new(0);
static CUR_BUFFER: AtomicU32 = AtomicU32::new(0);
static CUR_TRACK: AtomicI32 = AtomicI32::new(-1);

/// Install the JC3 music hook. Call once, only for `com.disney.JellyCar3`.
pub fn install_music_hook(env: &mut Environment) {
    let music: HostFunction = &(jc3_play_music as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_MUSIC_ADDR, "__touchHLE_JC3PlayMusic", music);
    log!(
        "Installed JellyCar 3 music hook (playMusic @ {:#x})",
        PLAY_MUSIC_ADDR
    );
}

/// Overwrite a Thumb function's entry with an 8-byte veneer that jumps to an
/// ARM host trampoline WITHOUT touching LR (so the trampoline's `bx lr` returns
/// to the caller):
///   ldr r3, [pc, #0]   ; 0x4b00
///   bx  r3             ; 0x4718
///   .word stub_addr
/// r3 is caller-clobbered under AAPCS; r0/r1 (the shim's inputs) are preserved.
fn patch_thumb_hook(env: &mut Environment, addr: u32, symbol: &'static str, hf: HostFunction) {
    let stub = env.dyld.create_guest_function(&mut env.mem, symbol, hf);
    let stub_addr = stub.addr_without_thumb_bit();
    let p0: MutPtr<u16> = Ptr::from_bits(addr);
    env.mem.write(p0, 0x4b00u16);
    let p1: MutPtr<u16> = Ptr::from_bits(addr + 2);
    env.mem.write(p1, 0x4718u16);
    let p2: MutPtr<u32> = Ptr::from_bits(addr + 4);
    env.mem.write(p2, stub_addr);
    env.cpu.invalidate_cache_range(addr, 8);
    log_dbg!("JC3 music: hooked {:#x} -> host trampoline {:#x}", addr, stub_addr);
}

/// Host implementation of `SoundManager::playMusic(int track)`.
/// ABI: r0 = SoundManager* (unused), r1 = track index (0-based).
fn jc3_play_music(env: &mut Environment) {
    let track = env.cpu.regs()[1] as i32;

    if !(0..=6).contains(&track) {
        stop_music(env);
        CUR_TRACK.store(-1, Ordering::Relaxed);
        return;
    }
    if CUR_TRACK.load(Ordering::Relaxed) == track {
        return;
    }

    let rel = format!("Content/Audio/Music/song{}.mp3", track + 1);
    let path = env.bundle.bundle_path().join(&rel);
    let Some((pcm, format, sample_rate)) = load_pcm(env, &path) else {
        return;
    };

    stop_music(env);

    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    let (source, buffer) = unsafe {
        let mut buffer: ALuint = 0;
        context.GenBuffers(1, &mut buffer);
        context.BufferData(
            buffer,
            format,
            pcm.as_ptr() as *const ALvoid,
            pcm.len() as ALsizei,
            sample_rate,
        );
        let mut source: ALuint = 0;
        context.GenSources(1, &mut source);
        context.Sourcei(source, al::AL_BUFFER, buffer as ALint);
        context.Sourcei(source, AL_LOOPING, 1);
        context.Sourcef(source, AL_GAIN, 1.0);
        context.SourcePlay(source);
        (source, buffer)
    };

    CUR_SOURCE.store(source, Ordering::Relaxed);
    CUR_BUFFER.store(buffer, Ordering::Relaxed);
    CUR_TRACK.store(track, Ordering::Relaxed);

    log!(
        "JC3 playMusic: track {} ({}) -> AL source {} ({} Hz, {} bytes)",
        track,
        rel,
        source,
        sample_rate,
        pcm.len()
    );
}

/// Open, decode and return interleaved 16-bit PCM + OpenAL format + sample rate.
fn load_pcm(env: &Environment, path: &GuestPathBuf) -> Option<(Vec<u8>, ALenum, ALsizei)> {
    let mut file = match AudioFile::open_for_reading(path, &env.fs) {
        Ok(f) => f,
        Err(e) => {
            log!("JC3 music: could not open {:?}: {:?}", path.as_str(), e);
            return None;
        }
    };
    let desc = file.audio_description();
    let byte_count = file.byte_count() as usize;
    let mut pcm = vec![0u8; byte_count];
    let read = match file.read_bytes(0, &mut pcm) {
        Ok(n) => n,
        Err(()) => {
            log!("JC3 music: decode failed for {:?}", path.as_str());
            return None;
        }
    };
    pcm.truncate(read);
    let format = if desc.channels_per_frame >= 2 {
        al::AL_FORMAT_STEREO16
    } else {
        al::AL_FORMAT_MONO16
    };
    Some((pcm, format, desc.sample_rate as ALsizei))
}

/// Stop + delete the current music source/buffer, if any.
fn stop_music(env: &mut Environment) {
    let source = CUR_SOURCE.swap(0, Ordering::Relaxed);
    let buffer = CUR_BUFFER.swap(0, Ordering::Relaxed);
    if source == 0 && buffer == 0 {
        return;
    }
    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    unsafe {
        if source != 0 {
            context.SourceStop(source);
            context.Sourcei(source, al::AL_BUFFER, 0);
            context.DeleteSources(1, &source);
        }
        if buffer != 0 {
            context.DeleteBuffers(1, &buffer);
        }
    }
}
