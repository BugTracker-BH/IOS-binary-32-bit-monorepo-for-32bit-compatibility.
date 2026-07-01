/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! JellyCar 3 audio shim (music + SFX), routed through touchHLE's OpenAL stack.
//!
//! FMOD fails to initialise under the emulator and JC3 disables its sound
//! system as a result. Rather than force FMOD init to "succeed" (which pushes
//! the game into an unhandled boost save-restore path and crashes), we bypass
//! the sound system entirely at a high level:
//!
//! * **Music:** hook `SoundManager::playMusic(int)` @ 0x101aac -> play
//!   `Content/Audio/Music/song<N>.mp3` looped.
//! * **SFX:** hook `SoundManager::playSoundFromGroup(int group, float vol)` @
//!   0x102434. The game calls this for one-shot effects (impacts, UI, win/lose,
//!   pickups, sproing, inflate/deflate) from gameplay code, independent of the
//!   (failed) FMOD system. We parse `Content/Audio/sounds.xml` ourselves to map
//!   group id -> wav files, pick one, and play it through OpenAL.
//!
//! Looping effects (engine/sticky/marker) go through the FMOD instance path,
//! which is dead without FMOD, so they are not covered here.
//!
//! Nothing in this module runs for any app other than `com.disney.JellyCar3`.

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

/// `Walaber::SoundManager::playMusic(int)` (Thumb).
const PLAY_MUSIC_ADDR: u32 = 0x101aac;
/// `Walaber::SoundManager::playSoundFromGroup(int group, float vol)` (Thumb).
const PLAY_SOUND_FROM_GROUP_ADDR: u32 = 0x102434;

/// Standard OpenAL 1.1 enum values not re-exported by openal_soft_wrapper.
const AL_LOOPING: ALenum = 0x1007;
const AL_GAIN: ALenum = 0x100A;

// --- Music state (single looping source) ---
static CUR_SOURCE: AtomicU32 = AtomicU32::new(0);
static CUR_BUFFER: AtomicU32 = AtomicU32::new(0);
static CUR_TRACK: AtomicI32 = AtomicI32::new(-1);

// --- SFX state ---
/// group id -> wav files (relative paths from sounds.xml, e.g. "Audio/Impact/hit1.wav").
static GROUP_FILES: OnceLock<HashMap<i32, Vec<String>>> = OnceLock::new();
/// filename -> decoded (pcm, format, sample_rate), so repeated hits don't re-decode.
fn pcm_cache() -> &'static Mutex<HashMap<String, (Vec<u8>, ALenum, ALsizei)>> {
    static S: OnceLock<Mutex<HashMap<String, (Vec<u8>, ALenum, ALsizei)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}
/// Currently-playing one-shot (source, buffer) pairs, reaped when finished.
fn sfx_pool() -> &'static Mutex<Vec<(ALuint, ALuint)>> {
    static S: OnceLock<Mutex<Vec<(ALuint, ALuint)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}
static RNG: AtomicU32 = AtomicU32::new(0x1234_5678);

fn next_rand() -> u32 {
    let mut x = RNG.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    RNG.store(x, Ordering::Relaxed);
    x
}

/// Install JC3 audio hooks. Call once, only for `com.disney.JellyCar3`.
pub fn install_audio_hooks(env: &mut Environment) {
    let _ = GROUP_FILES.set(parse_sound_groups(env));

    let music: HostFunction = &(jc3_play_music as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_MUSIC_ADDR, "__touchHLE_JC3PlayMusic", music);

    let sfx: HostFunction = &(jc3_play_sound_from_group as fn(&mut Environment));
    patch_thumb_hook(
        env,
        PLAY_SOUND_FROM_GROUP_ADDR,
        "__touchHLE_JC3PlaySoundFromGroup",
        sfx,
    );

    let groups = GROUP_FILES.get().map(|g| g.len()).unwrap_or(0);
    log!(
        "Installed JellyCar 3 audio hooks (music @ {:#x}, SFX playSoundFromGroup @ {:#x}, \
         {} sound groups parsed from sounds.xml)",
        PLAY_MUSIC_ADDR,
        PLAY_SOUND_FROM_GROUP_ADDR,
        groups
    );
}

/// Overwrite a Thumb function's entry with an 8-byte veneer that jumps to an
/// ARM host trampoline WITHOUT touching LR (so the trampoline's `bx lr` returns
/// to the caller):
///   ldr r3, [pc, #0]   ; 0x4b00
///   bx  r3             ; 0x4718
///   .word stub_addr
/// r3 is caller-clobbered under AAPCS; r0-r2 (all inputs our shims read) are
/// preserved.
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

/// Host `SoundManager::playSoundFromGroup(int group, float vol)`.
/// ABI: r0 = this (unused), r1 = group id, r2 = volume (float bits, softfp).
fn jc3_play_sound_from_group(env: &mut Environment) {
    let group = env.cpu.regs()[1] as i32;
    let vol = f32::from_bits(env.cpu.regs()[2]);
    let vol = if vol.is_finite() { vol.clamp(0.0, 1.0) } else { 1.0 };
    if vol <= 0.0 {
        return;
    }

    // Pick a wav from the group (mirrors the game's random selection).
    let name = {
        let Some(files) = GROUP_FILES.get().and_then(|g| g.get(&group)) else {
            return;
        };
        if files.is_empty() {
            return;
        }
        files[(next_rand() as usize) % files.len()].clone()
    };

    // Decode (cached).
    if !pcm_cache().lock().unwrap().contains_key(&name) {
        let Some(path) = resolve_sound_path(env, &name) else {
            log!("JC3 SFX: file not found for {:?}", name);
            return;
        };
        match load_pcm(env, &path) {
            Some(dec) => {
                pcm_cache().lock().unwrap().insert(name.clone(), dec);
            }
            None => return,
        }
    }

    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    unsafe {
        // Reap finished one-shots.
        let mut pool = sfx_pool().lock().unwrap();
        let mut i = 0;
        while i < pool.len() {
            let (s, b) = pool[i];
            let mut state: ALint = 0;
            context.GetSourcei(s, al::AL_SOURCE_STATE, &mut state);
            if state != al::AL_PLAYING {
                context.DeleteSources(1, &s);
                context.DeleteBuffers(1, &b);
                pool.remove(i);
            } else {
                i += 1;
            }
        }

        let cache = pcm_cache().lock().unwrap();
        let (pcm, fmt, rate) = cache.get(&name).unwrap();
        let mut buffer: ALuint = 0;
        context.GenBuffers(1, &mut buffer);
        context.BufferData(
            buffer,
            *fmt,
            pcm.as_ptr() as *const ALvoid,
            pcm.len() as ALsizei,
            *rate,
        );
        let mut source: ALuint = 0;
        context.GenSources(1, &mut source);
        context.Sourcei(source, al::AL_BUFFER, buffer as ALint);
        context.Sourcef(source, AL_GAIN, vol);
        context.SourcePlay(source);
        pool.push((source, buffer));
    }

    log!("JC3 SFX: group {} -> {} (vol {:.2})", group, name, vol);
}

/// Try the sensible bundle-relative locations for a sounds.xml filename.
fn resolve_sound_path(env: &Environment, name: &str) -> Option<GuestPathBuf> {
    let bp = env.bundle.bundle_path();
    for cand in [format!("Content/{}", name), name.to_string()] {
        let p = bp.join(&cand);
        if env.fs.is_file(&p) {
            return Some(p);
        }
    }
    None
}

/// Parse `Content/Audio/sounds.xml` -> map of group id to its wav filenames.
fn parse_sound_groups(env: &Environment) -> HashMap<i32, Vec<String>> {
    let mut map: HashMap<i32, Vec<String>> = HashMap::new();
    let path = env.bundle.bundle_path().join("Content/Audio/sounds.xml");
    let bytes = match env.fs.read(&path) {
        Ok(b) => b,
        Err(()) => {
            log!("JC3 SFX: could not read {:?}", path.as_str());
            return map;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut cur: Option<i32> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("<Group ") {
            cur = attr(rest, "id").and_then(|s| s.parse::<i32>().ok());
        } else if l.starts_with("</Group>") || l.starts_with("<Music") {
            cur = None;
        } else if let Some(rest) = l.strip_prefix("<Sound ") {
            if let (Some(g), Some(fname)) = (cur, attr(rest, "filename")) {
                map.entry(g).or_default().push(fname);
            }
        }
    }
    map
}

/// Extract a simple `key="value"` XML attribute.
fn attr(s: &str, key: &str) -> Option<String> {
    let pat = format!("{}=\"", key);
    let start = s.find(&pat)? + pat.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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
