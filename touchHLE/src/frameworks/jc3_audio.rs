/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! JellyCar 3 music shim — Stage 1 of the FMOD-to-OpenAL audio bridge.
//!
//! FMOD is fully stubbed for JC3 (see the JC3 init block in `environment.rs`),
//! so the game runs silently. This module restores *music* by hooking the
//! highest-level entry point, `Walaber::SoundManager::playMusic(int track)` at
//! guest address `0x101aac`, and playing the matching MP3 through touchHLE's
//! existing OpenAL stack instead of FMOD.
//!
//! Nothing in this module runs for any app other than `com.disney.JellyCar3`:
//! [install_music_hook] is only called from the JC3-gated init block, and it is
//! the only entry point here.
//!
//! SFX (the lower-level `FMOD::System`/`Channel` calls) are Stage 2 and are not
//! handled here.

use crate::audio::openal as al;
use crate::audio::openal::al_types::*;
use crate::audio::AudioFile;
use crate::dyld::HostFunction;
use crate::mem::{MutPtr, Ptr};
use crate::Environment;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

/// Guest address of `Walaber::SoundManager::playMusic(int)` (Thumb code).
const PLAY_MUSIC_ADDR: u32 = 0x101aac;

/// Standard OpenAL 1.1 enum values that the `openal_soft_wrapper` crate does
/// not re-export (it only defines the handful it needs). Values per `al.h`.
const AL_LOOPING: ALenum = 0x1007;
const AL_GAIN: ALenum = 0x100A;

/// The currently playing OpenAL source / buffer names (0 = none), and the
/// track index that is playing (-1 = none). JC3 is single-threaded from our
/// perspective (the hook only runs on the guest's main thread), so plain
/// atomics are sufficient to carry this state between calls.
static CUR_SOURCE: AtomicU32 = AtomicU32::new(0);
static CUR_BUFFER: AtomicU32 = AtomicU32::new(0);
static CUR_TRACK: AtomicI32 = AtomicI32::new(-1);

/// Install the JC3 music hook. Call exactly once, only for
/// `com.disney.JellyCar3`, after the FMOD bypass stubs are written.
pub fn install_music_hook(env: &mut Environment) {
    // Mint an ARM-mode host trampoline: `svc <idx>; bx lr`. Invoking it runs
    // [jc3_play_music] and then returns to LR — i.e. to the game code that
    // called playMusic.
    let hf: HostFunction = &(jc3_play_music as fn(&mut Environment));
    let stub = env
        .dyld
        .create_guest_function(&mut env.mem, "__touchHLE_JC3PlayMusic", hf);
    let stub_addr = stub.addr_without_thumb_bit();

    // playMusic is Thumb code, but the trampoline is ARM. Overwrite playMusic's
    // entry with an 8-byte Thumb veneer that switches to ARM WITHOUT touching
    // LR (so the trampoline's `bx lr` returns to playMusic's caller):
    //   ldr r3, [pc, #0]   ; 0x4b00  -> r3 = stub_addr (literal at +4)
    //   bx  r3             ; 0x4718  -> branch to ARM stub, switches to ARM
    //   .word stub_addr    ; ARM address (bit 0 = 0 -> ARM state)
    // r3 is caller-clobbered under AAPCS; r0 (this) and r1 (track) — the only
    // inputs the host shim reads — are preserved.
    let p0: MutPtr<u16> = Ptr::from_bits(PLAY_MUSIC_ADDR);
    env.mem.write(p0, 0x4b00u16);
    let p1: MutPtr<u16> = Ptr::from_bits(PLAY_MUSIC_ADDR + 2);
    env.mem.write(p1, 0x4718u16);
    let p2: MutPtr<u32> = Ptr::from_bits(PLAY_MUSIC_ADDR + 4);
    env.mem.write(p2, stub_addr);
    env.cpu.invalidate_cache_range(PLAY_MUSIC_ADDR, 8);

    log!(
        "Installed JellyCar 3 playMusic hook @ {:#x} -> host trampoline {:#x}",
        PLAY_MUSIC_ADDR,
        stub_addr
    );
}

/// Host implementation of `SoundManager::playMusic(int track)`.
///
/// ABI (ARM AAPCS / C++ thiscall): r0 = `SoundManager*` (unused), r1 = track
/// index (0-based). Returns void; we leave r0 untouched.
fn jc3_play_music(env: &mut Environment) {
    let track = env.cpu.regs()[1] as i32;

    // JC3 ships song1.mp3 .. song7.mp3 (track index 0..=6). Any other value
    // (e.g. a "stop music" sentinel) just stops playback.
    if !(0..=6).contains(&track) {
        stop_current(env);
        CUR_TRACK.store(-1, Ordering::Relaxed);
        return;
    }

    // Already playing this track? Leave it running to avoid restart stutter
    // (the game may re-issue playMusic for the current track).
    if CUR_TRACK.load(Ordering::Relaxed) == track {
        return;
    }

    // Build "<bundle>/Content/Audio/Music/song<N>.mp3".
    let rel = format!("Content/Audio/Music/song{}.mp3", track + 1);
    let path = env.bundle.bundle_path().join(&rel);

    // Decode the whole file to interleaved 16-bit PCM up front.
    let mut file = match AudioFile::open_for_reading(&path, &env.fs) {
        Ok(f) => f,
        Err(e) => {
            log!(
                "JC3 playMusic: could not open {:?}: {:?}",
                path.as_str(),
                e
            );
            return;
        }
    };
    let desc = file.audio_description();
    let byte_count = file.byte_count() as usize;
    let mut pcm = vec![0u8; byte_count];
    let read = match file.read_bytes(0, &mut pcm) {
        Ok(n) => n,
        Err(()) => {
            log!("JC3 playMusic: decode failed for {:?}", path.as_str());
            return;
        }
    };
    pcm.truncate(read);

    let format = if desc.channels_per_frame >= 2 {
        al::AL_FORMAT_STEREO16
    } else {
        al::AL_FORMAT_MONO16
    };
    let sample_rate = desc.sample_rate as ALsizei;
    let channels = desc.channels_per_frame;

    // Stop / free whatever was playing before uploading the new track.
    stop_current(env);

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
        context.Sourcei(source, AL_LOOPING, 1 /* AL_TRUE */);
        context.Sourcef(source, AL_GAIN, 1.0);
        context.SourcePlay(source);
        (source, buffer)
    };

    CUR_SOURCE.store(source, Ordering::Relaxed);
    CUR_BUFFER.store(buffer, Ordering::Relaxed);
    CUR_TRACK.store(track, Ordering::Relaxed);

    log!(
        "JC3 playMusic: track {} ({}) -> AL source {} buffer {} ({} Hz, {} ch, {} bytes)",
        track,
        rel,
        source,
        buffer,
        sample_rate,
        channels,
        pcm.len()
    );
}

/// Stop and delete the current AL source + buffer, if any.
fn stop_current(env: &mut Environment) {
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
            // Detach the buffer before deleting it.
            context.Sourcei(source, al::AL_BUFFER, 0);
            context.DeleteSources(1, &source);
        }
        if buffer != 0 {
            context.DeleteBuffers(1, &buffer);
        }
    }
}
