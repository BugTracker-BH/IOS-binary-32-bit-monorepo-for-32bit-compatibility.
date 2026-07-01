/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! JellyCar 3 audio shim — the full FMOD-to-OpenAL bridge (music + SFX).
//!
//! FMOD is stubbed for JC3, so the game runs silently. This module restores all
//! audio by hooking `Walaber` engine entry points and routing playback through
//! touchHLE's existing OpenAL stack:
//!
//! * **Music:** `SoundManager::playMusic(int)` @ 0x101aac -> plays
//!   `Content/Audio/Music/song<N>.mp3` looped.
//! * **SFX:** every sound effect flows through
//!   `SoundEffectInstance::play(float)` @ 0xfbde8. An instance's sound is the
//!   `FMOD::Sound*` at `[this+4]`, produced by `FMOD::System::createSound` @
//!   0x1b2944 inside `SoundManager::addSound`. We take ownership of createSound/
//!   createStream so each returns a *unique* (zeroed, deref-safe) sound object
//!   and we record `sound_ptr -> filename`. Then `play`/`stop` look up
//!   `[this+4]`, decode the wav, and play it (looping per `sounds.xml`) through
//!   OpenAL. `SoundEffectInstance::stop()` @ 0xfa59c stops it.
//!
//! Nothing here runs for any app other than `com.disney.JellyCar3`.

use crate::audio::openal as al;
use crate::audio::openal::al_types::*;
use crate::audio::AudioFile;
use crate::dyld::HostFunction;
use crate::fs::GuestPathBuf;
use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::Environment;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

/// `Walaber::SoundManager::playMusic(int)` (Thumb).
const PLAY_MUSIC_ADDR: u32 = 0x101aac;
/// `Walaber::SoundEffectInstance::play(float)` (Thumb).
const PLAY_SFX_ADDR: u32 = 0xfbde8;
/// `Walaber::SoundEffectInstance::stop()` (Thumb).
const STOP_SFX_ADDR: u32 = 0xfa59c;
/// `FMOD::System::createSound(...)` (Thumb; currently a bypass stub).
const CREATE_SOUND_ADDR: u32 = 0x1b2944;
/// `FMOD::System::createStream(...)` (Thumb; currently a bypass stub).
const CREATE_STREAM_ADDR: u32 = 0x1b28f8;
/// Offset of the `FMOD::Sound*` field inside a `SoundEffectInstance`.
const INSTANCE_SOUND_OFFSET: u32 = 4;

/// Standard OpenAL 1.1 enum values not re-exported by openal_soft_wrapper.
const AL_LOOPING: ALenum = 0x1007;
const AL_GAIN: ALenum = 0x100A;

// --- Music state (single looping source) ---
static CUR_SOURCE: AtomicU32 = AtomicU32::new(0);
static CUR_BUFFER: AtomicU32 = AtomicU32::new(0);
static CUR_TRACK: AtomicI32 = AtomicI32::new(-1);

// --- SFX state (lazily-initialised because HashMap::new isn't const) ---
/// sound object pointer -> the filename createSound was asked to load.
fn sound_files() -> &'static Mutex<HashMap<u32, String>> {
    static S: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}
/// SoundEffectInstance pointer -> its currently playing (source, buffer).
fn instance_src() -> &'static Mutex<HashMap<u32, (ALuint, ALuint)>> {
    static S: OnceLock<Mutex<HashMap<u32, (ALuint, ALuint)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}
/// filename -> decoded (pcm, format, sample_rate), cached so repeated hits don't
/// re-decode.
fn pcm_cache() -> &'static Mutex<HashMap<String, (Vec<u8>, ALenum, ALsizei)>> {
    static S: OnceLock<Mutex<HashMap<String, (Vec<u8>, ALenum, ALsizei)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}
/// Basenames of sounds that should loop (from `sounds.xml` `loop="true"` groups).
static LOOP_BASENAMES: OnceLock<Vec<String>> = OnceLock::new();

/// Install all JC3 audio hooks. Call once, only for `com.disney.JellyCar3`,
/// AFTER the FMOD bypass stubs are written (so our createSound/createStream
/// hooks override the fixed-dummy stubs).
pub fn install_audio_hooks(env: &mut Environment) {
    let _ = LOOP_BASENAMES.set(parse_loop_basenames(env));

    let music: HostFunction = &(jc3_play_music as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_MUSIC_ADDR, "__touchHLE_JC3PlayMusic", music);

    let cs: HostFunction = &(jc3_create_sound as fn(&mut Environment));
    patch_thumb_hook(env, CREATE_SOUND_ADDR, "__touchHLE_JC3CreateSound", cs);
    patch_thumb_hook(env, CREATE_STREAM_ADDR, "__touchHLE_JC3CreateStream", cs);

    let play: HostFunction = &(jc3_sfx_play as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_SFX_ADDR, "__touchHLE_JC3SfxPlay", play);
    let stop: HostFunction = &(jc3_sfx_stop as fn(&mut Environment));
    patch_thumb_hook(env, STOP_SFX_ADDR, "__touchHLE_JC3SfxStop", stop);

    log!(
        "Installed JellyCar 3 audio hooks (music @ {:#x}, SFX play @ {:#x}/stop @ {:#x}, \
         createSound @ {:#x}/{:#x})",
        PLAY_MUSIC_ADDR,
        PLAY_SFX_ADDR,
        STOP_SFX_ADDR,
        CREATE_SOUND_ADDR,
        CREATE_STREAM_ADDR
    );
}

/// Overwrite a Thumb function's entry with an 8-byte veneer that jumps to an
/// ARM host trampoline WITHOUT touching LR (so the trampoline's `bx lr` returns
/// to the caller):
///   ldr r3, [pc, #0]   ; 0x4b00
///   bx  r3             ; 0x4718
///   .word stub_addr    ; ARM address
/// r3 is caller-clobbered under AAPCS; r0-r2 (all inputs our shims read) are
/// preserved, as are stack args.
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

/// Read a NUL-terminated guest C string (bounded).
fn read_cstr(env: &Environment, addr: u32) -> String {
    let mut bytes = Vec::new();
    let mut p = addr;
    for _ in 0..1024 {
        let cp: ConstPtr<u8> = Ptr::from_bits(p);
        let b: u8 = env.mem.read(cp);
        if b == 0 {
            break;
        }
        bytes.push(b);
        p += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Host `FMOD::System::createSound` / `createStream`.
/// ABI: r0=System*, r1=const char* name, r2=mode, r3=exinfo, [sp]=FMOD::Sound**.
/// We hand back a unique zeroed sound object and record its pointer -> filename.
fn jc3_create_sound(env: &mut Environment) {
    let name_ptr = env.cpu.regs()[1];
    let sp = env.cpu.regs()[13];
    // 5th argument (the out `FMOD::Sound**`) is on the stack at [sp].
    let sp_ptr: ConstPtr<u32> = Ptr::from_bits(sp);
    let out_ptr: u32 = env.mem.read(sp_ptr);
    let name = read_cstr(env, name_ptr);

    // Allocate a unique, zeroed pseudo-Sound object. Zeroed (like the old fixed
    // dummy) so any incidental FMOD deref reads zeros safely; unique so we can
    // map it back to a filename at play() time.
    let buf = env.mem.alloc(0x40);
    let bits = buf.to_bits();
    for i in 0..0x10u32 {
        let z: MutPtr<u32> = Ptr::from_bits(bits + i * 4);
        env.mem.write(z, 0u32);
    }
    if out_ptr != 0 {
        let outp: MutPtr<u32> = Ptr::from_bits(out_ptr);
        env.mem.write(outp, bits);
    }
    sound_files().lock().unwrap().insert(bits, name.clone());
    env.cpu.regs_mut()[0] = 0; // FMOD_OK
    log_dbg!("JC3 createSound {:#x} -> {}", bits, name);
}

/// Host `SoundEffectInstance::play(float vol)`.
/// ABI: r0 = instance*, r1 = volume (float bits, softfp).
fn jc3_sfx_play(env: &mut Environment) {
    let this = env.cpu.regs()[0];
    let vol = f32::from_bits(env.cpu.regs()[1]);
    let snd_field: ConstPtr<u32> = Ptr::from_bits(this + INSTANCE_SOUND_OFFSET);
    let sound_ptr: u32 = env.mem.read(snd_field);

    let name = match sound_files().lock().unwrap().get(&sound_ptr) {
        Some(s) => s.clone(),
        None => return, // sound object we didn't create; nothing to play
    };

    if !vol.is_finite() {
        return;
    }
    // The game uses play(0.0) to mean "stop".
    if vol <= 0.0 {
        stop_instance(env, this);
        return;
    }
    let vol = vol.min(1.0);

    // Decode (cached).
    if !pcm_cache().lock().unwrap().contains_key(&name) {
        let path = match resolve_sound_path(env, &name) {
            Some(p) => p,
            None => {
                log!("JC3 SFX: file not found for {:?}", name);
                return;
            }
        };
        match load_pcm(env, &path) {
            Some(dec) => {
                pcm_cache().lock().unwrap().insert(name.clone(), dec);
            }
            None => return,
        }
    }

    let looping = is_loop(&name);

    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    unsafe {
        let mut inst = instance_src().lock().unwrap();

        // Reap finished one-shots (any instance other than this one).
        let keys: Vec<u32> = inst.keys().copied().collect();
        for k in keys {
            if k == this {
                continue;
            }
            let (s, b) = inst[&k];
            let mut state: ALint = 0;
            context.GetSourcei(s, al::AL_SOURCE_STATE, &mut state);
            if state != al::AL_PLAYING {
                context.DeleteSources(1, &s);
                context.DeleteBuffers(1, &b);
                inst.remove(&k);
            }
        }

        // Retrigger: stop any sound currently playing on this instance.
        if let Some((s, b)) = inst.remove(&this) {
            context.SourceStop(s);
            context.Sourcei(s, al::AL_BUFFER, 0);
            context.DeleteSources(1, &s);
            context.DeleteBuffers(1, &b);
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
        context.Sourcei(source, AL_LOOPING, if looping { 1 } else { 0 });
        context.Sourcef(source, AL_GAIN, vol);
        context.SourcePlay(source);
        inst.insert(this, (source, buffer));
    }

    log_dbg!(
        "JC3 SFX play: inst {:#x} snd {:#x} -> {} (vol {:.2}{})",
        this,
        sound_ptr,
        name,
        vol,
        if looping { ", loop" } else { "" }
    );
}

/// Host `SoundEffectInstance::stop()`. ABI: r0 = instance*.
fn jc3_sfx_stop(env: &mut Environment) {
    let this = env.cpu.regs()[0];
    stop_instance(env, this);
}

fn stop_instance(env: &mut Environment, this: u32) {
    let entry = instance_src().lock().unwrap().remove(&this);
    if let Some((source, buffer)) = entry {
        let context = env
            .framework_state
            .audio_toolbox
            .make_al_context_current(env.openal_manager.as_mut());
        unsafe {
            context.SourceStop(source);
            context.Sourcei(source, al::AL_BUFFER, 0);
            context.DeleteSources(1, &source);
            context.DeleteBuffers(1, &buffer);
        }
    }
}

/// True if `name`'s basename belongs to a `loop="true"` group in sounds.xml.
fn is_loop(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    LOOP_BASENAMES
        .get()
        .map_or(false, |v| v.iter().any(|b| b == base))
}

/// Try the sensible bundle-relative locations for a createSound filename.
fn resolve_sound_path(env: &Environment, name: &str) -> Option<GuestPathBuf> {
    let bp = env.bundle.bundle_path();
    for cand in [name.to_string(), format!("Content/{}", name)] {
        let p = bp.join(&cand);
        if env.fs.is_file(&p) {
            return Some(p);
        }
    }
    None
}

/// Parse `Content/Audio/sounds.xml` and collect the basenames of sounds that
/// belong to `loop="true"` groups (engine, sticky-tire loop, alarm, marker...).
fn parse_loop_basenames(env: &Environment) -> Vec<String> {
    let mut out = Vec::new();
    let path = env.bundle.bundle_path().join("Content/Audio/sounds.xml");
    let bytes = match env.fs.read(&path) {
        Ok(b) => b,
        Err(()) => return out,
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut cur_loop = false;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("<Group ") {
            cur_loop = attr(rest, "loop").as_deref() == Some("true");
        } else if l.starts_with("</Group>") || l.starts_with("<Music") {
            cur_loop = false;
        } else if let Some(rest) = l.strip_prefix("<Sound ") {
            if cur_loop {
                if let Some(fname) = attr(rest, "filename") {
                    let base = fname.rsplit('/').next().unwrap_or(&fname).to_string();
                    out.push(base);
                }
            }
        }
    }
    out
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
