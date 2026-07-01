/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! JellyCar 3 audio shim — the FMOD-to-OpenAL bridge.
//!
//! FMOD is fully stubbed for JC3 (see the JC3 init block in `environment.rs`),
//! so the game runs silently. This module restores audio by hooking the two
//! high-level `Walaber::SoundManager` entry points and routing playback through
//! touchHLE's existing OpenAL stack instead of FMOD:
//!
//! * **Stage 1 — music:** `SoundManager::playMusic(int track)` @ `0x101aac`.
//!   Plays `Content/Audio/Music/song<N>.mp3` looped.
//! * **Stage 2 — SFX:** `SoundManager::playSoundFromGroup(int group, float vol)`
//!   @ `0x102434`. The group→file mapping is read from `Content/Audio/sounds.xml`
//!   (the same manifest the game itself parses); a random file from the group is
//!   played once through OpenAL at the requested volume.
//!
//! Nothing here runs for any app other than `com.disney.JellyCar3`:
//! [install_audio_hooks] is only called from the JC3-gated init block.

use crate::audio::openal as al;
use crate::audio::openal::al_types::*;
use crate::audio::AudioFile;
use crate::dyld::HostFunction;
use crate::fs::GuestPathBuf;
use crate::mem::{MutPtr, Ptr};
use crate::Environment;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

/// Guest address of `Walaber::SoundManager::playMusic(int)` (Thumb code).
const PLAY_MUSIC_ADDR: u32 = 0x101aac;
/// Guest address of `Walaber::SoundManager::playSoundFromGroup(int, float)`
/// (Thumb code).
const PLAY_SFX_ADDR: u32 = 0x102434;

/// Standard OpenAL 1.1 enum values that the `openal_soft_wrapper` crate does
/// not re-export (it only defines the handful it needs). Values per `al.h`.
const AL_LOOPING: ALenum = 0x1007;
const AL_GAIN: ALenum = 0x100A;

/// The currently playing music OpenAL source / buffer names (0 = none), and the
/// track index that is playing (-1 = none). JC3 is single-threaded from our
/// perspective (the hooks only run on the guest's main thread), so plain
/// atomics are sufficient to carry this state between calls.
static CUR_SOURCE: AtomicU32 = AtomicU32::new(0);
static CUR_BUFFER: AtomicU32 = AtomicU32::new(0);
static CUR_TRACK: AtomicI32 = AtomicI32::new(-1);

/// group id -> list of sound file paths (guest-relative, e.g.
/// "Content/Audio/Impact/hit1.wav"), parsed once from `sounds.xml`.
static SFX_GROUPS: OnceLock<HashMap<i32, Vec<String>>> = OnceLock::new();

/// Currently live one-shot SFX (source, buffer) pairs. Reaped lazily on each
/// new SFX so finished sounds don't leak OpenAL objects.
static SFX_POOL: Mutex<Vec<(ALuint, ALuint)>> = Mutex::new(Vec::new());

/// Tiny xorshift RNG for picking a random sound within a group.
static RNG: AtomicU32 = AtomicU32::new(0x9e3779b9);
fn rand_index(n: usize) -> usize {
    let mut x = RNG.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    RNG.store(x, Ordering::Relaxed);
    (x as usize) % n.max(1)
}

/// Install the JC3 audio hooks. Call exactly once, only for
/// `com.disney.JellyCar3`, after the FMOD bypass stubs are written.
pub fn install_audio_hooks(env: &mut Environment) {
    // Stage 2 needs the group->file table before any SFX can play.
    let table = parse_sounds_xml(env);
    let group_count = table.len();
    let _ = SFX_GROUPS.set(table);

    let music: HostFunction = &(jc3_play_music as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_MUSIC_ADDR, "__touchHLE_JC3PlayMusic", music);

    let sfx: HostFunction = &(jc3_play_sound_from_group as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_SFX_ADDR, "__touchHLE_JC3PlaySoundFromGroup", sfx);

    log!(
        "Installed JellyCar 3 audio hooks (music @ {:#x}, SFX @ {:#x}, {} sound groups)",
        PLAY_MUSIC_ADDR,
        PLAY_SFX_ADDR,
        group_count
    );
}

/// Overwrite a Thumb function's entry with an 8-byte veneer that jumps to an
/// ARM-mode host trampoline (`svc <idx>; bx lr`) WITHOUT touching LR, so the
/// trampoline's `bx lr` returns to the game code that called the function:
///   ldr r3, [pc, #0]   ; 0x4b00  -> r3 = stub_addr (literal at +4)
///   bx  r3             ; 0x4718  -> branch to ARM stub, switches to ARM state
///   .word stub_addr    ; ARM address (bit 0 = 0 -> ARM state)
/// r3 is caller-clobbered under AAPCS; r0/r1/r2 (the only inputs the shims read)
/// are preserved.
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

    log_dbg!("JC3 audio: hooked {:#x} -> host trampoline {:#x}", addr, stub_addr);
}

/// Parse `Content/Audio/sounds.xml` into a group id -> file paths map. The
/// integer `id` attributes match the argument passed to `playSoundFromGroup`.
fn parse_sounds_xml(env: &Environment) -> HashMap<i32, Vec<String>> {
    let mut groups: HashMap<i32, Vec<String>> = HashMap::new();
    let path = env.bundle.bundle_path().join("Content/Audio/sounds.xml");
    let bytes = match env.fs.read(&path) {
        Ok(b) => b,
        Err(()) => {
            log!("JC3 SFX: could not read {:?}; SFX disabled", path.as_str());
            return groups;
        }
    };
    let text = String::from_utf8_lossy(&bytes);

    let mut cur: Option<i32> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("<Group ") {
            cur = attr(rest, "id").and_then(|s| s.parse::<i32>().ok());
            if let Some(id) = cur {
                groups.entry(id).or_default();
            }
        } else if l.starts_with("</Group>") || l.starts_with("<Music") {
            // Music tracks are handled by the playMusic hook; ignore them here.
            cur = None;
        } else if let Some(rest) = l.strip_prefix("<Sound ") {
            if let (Some(id), Some(fname)) = (cur, attr(rest, "filename")) {
                // Manifest paths are relative to the "Content" directory.
                groups.entry(id).or_default().push(format!("Content/{}", fname));
            }
        }
    }
    groups
}

/// Extract the value of a simple `key="value"` XML attribute from `s`.
fn attr(s: &str, key: &str) -> Option<String> {
    let pat = format!("{}=\"", key);
    let start = s.find(&pat)? + pat.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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

    let Some((pcm, format, sample_rate)) = load_pcm(env, &path) else {
        return;
    };

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
        "JC3 playMusic: track {} ({}) -> AL source {} buffer {} ({} Hz, {} bytes)",
        track,
        rel,
        source,
        buffer,
        sample_rate,
        pcm.len()
    );
}

/// Host implementation of `SoundManager::playSoundFromGroup(int group, float vol)`.
///
/// ABI (ARM AAPCS, iOS softfp): r0 = `SoundManager*` (unused), r1 = group id,
/// r2 = volume as raw float bits. Returns void.
fn jc3_play_sound_from_group(env: &mut Environment) {
    let group = env.cpu.regs()[1] as i32;
    let mut vol = f32::from_bits(env.cpu.regs()[2]);
    if !vol.is_finite() || !(0.0..=1.0).contains(&vol) {
        vol = 1.0;
    }

    let Some(groups) = SFX_GROUPS.get() else {
        return;
    };
    let Some(files) = groups.get(&group) else {
        log_dbg!("JC3 SFX: unknown group {}", group);
        return;
    };
    if files.is_empty() {
        return;
    }
    let rel = files[rand_index(files.len())].clone();
    let path = env.bundle.bundle_path().join(&rel);

    let Some((pcm, format, sample_rate)) = load_pcm(env, &path) else {
        return;
    };

    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    let source = unsafe {
        let mut pool = SFX_POOL.lock().unwrap();

        // Reap finished one-shots so we don't leak OpenAL sources/buffers.
        let mut i = 0;
        while i < pool.len() {
            let (src, buf) = pool[i];
            let mut state: ALint = 0;
            context.GetSourcei(src, al::AL_SOURCE_STATE, &mut state);
            if state == al::AL_PLAYING {
                i += 1;
            } else {
                context.DeleteSources(1, &src);
                context.DeleteBuffers(1, &buf);
                pool.swap_remove(i);
            }
        }

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
        context.Sourcef(source, AL_GAIN, vol);
        context.SourcePlay(source);
        pool.push((source, buffer));
        source
    };

    log!(
        "JC3 SFX: group {} -> {} -> AL source {} ({} Hz, {} bytes, vol {:.2})",
        group,
        rel,
        source,
        sample_rate,
        pcm.len(),
        vol
    );
}

/// Open, decode and return interleaved 16-bit PCM plus its OpenAL format and
/// sample rate. Returns `None` (and logs) if the file can't be read/decoded.
fn load_pcm(env: &Environment, path: &GuestPathBuf) -> Option<(Vec<u8>, ALenum, ALsizei)> {
    let mut file = match AudioFile::open_for_reading(path, &env.fs) {
        Ok(f) => f,
        Err(e) => {
            log!("JC3 audio: could not open {:?}: {:?}", path.as_str(), e);
            return None;
        }
    };
    let desc = file.audio_description();
    let byte_count = file.byte_count() as usize;
    let mut pcm = vec![0u8; byte_count];
    let read = match file.read_bytes(0, &mut pcm) {
        Ok(n) => n,
        Err(()) => {
            log!("JC3 audio: decode failed for {:?}", path.as_str());
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

/// Stop and delete the current music AL source + buffer, if any.
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
