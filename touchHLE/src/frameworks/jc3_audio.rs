/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! JellyCar 3 audio bridge — routes the game's audio through touchHLE's OpenAL
//! stack instead of FMOD (which is stubbed for JC3, see the JC3 init block in
//! `environment.rs`). JC3-only; installed from that JC3-gated block.
//!
//! * **Music:** `SoundManager::playMusic(int track)` @ `0x101aac` -> plays the
//!   track's MP3 looped.
//! * **SFX (full):** the FMOD "sound" handle is intercepted at
//!   `FMOD::System::createSound` @ `0x1b2944` (we hand back a unique handle
//!   mapped to the wav path), and playback is driven by hooking
//!   `Walaber::SoundEffectInstance::play(float)` @ `0xfbde8` /
//!   `::stop()` @ `0xfa59c` — the single funnel every UI and gameplay sound
//!   passes through. Each instance stores its `FMOD::Sound*` (our handle) at
//!   offset +4, so `play` looks up the wav and plays it through OpenAL.

use crate::audio::openal as al;
use crate::audio::openal::al_types::*;
use crate::audio::AudioFile;
use crate::cpu::Cpu;
use crate::dyld::HostFunction;
use crate::fs::GuestPathBuf;
use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::Environment;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

const PLAY_MUSIC_ADDR: u32 = 0x101aac;
const CREATE_SOUND_ADDR: u32 = 0x1b2944;
const SEI_PLAY_ADDR: u32 = 0xfbde8; // SoundEffectInstance::play(float)
const SEI_STOP_ADDR: u32 = 0xfa59c; // SoundEffectInstance::stop()

/// Standard OpenAL 1.1 enum values not re-exported by openal_soft_wrapper.
const AL_LOOPING: ALenum = 0x1007;
const AL_GAIN: ALenum = 0x100A;
const AL_SOURCE_STATE: ALenum = 0x1010;
const AL_PLAYING: ALenum = 0x1012;

/// FMOD_MODE loop bits (FMOD_LOOP_NORMAL | FMOD_LOOP_BIDI).
const FMOD_LOOP_BITS: u32 = 0x2 | 0x4;

// --- music state ---
static CUR_SOURCE: AtomicU32 = AtomicU32::new(0);
static CUR_BUFFER: AtomicU32 = AtomicU32::new(0);
static CUR_TRACK: AtomicI32 = AtomicI32::new(-1);

/// FMOD Sound handle (our alloc) -> (wav path from createSound, loop flag).
static SOUND_MAP: OnceLock<Mutex<HashMap<u32, (String, bool)>>> = OnceLock::new();
/// SoundEffectInstance* -> (AL source, AL buffer, sound handle currently playing).
static INSTANCE_SRC: OnceLock<Mutex<HashMap<u32, (ALuint, ALuint, u32)>>> = OnceLock::new();

fn sound_map() -> &'static Mutex<HashMap<u32, (String, bool)>> {
    SOUND_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}
fn instance_src() -> &'static Mutex<HashMap<u32, (ALuint, ALuint, u32)>> {
    INSTANCE_SRC.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Install the JC3 audio hooks. Call once, only for `com.disney.JellyCar3`.
pub fn install_audio_hooks(env: &mut Environment) {
    let music: HostFunction = &(jc3_play_music as fn(&mut Environment));
    patch_thumb_hook(env, PLAY_MUSIC_ADDR, "__touchHLE_JC3PlayMusic", music);

    let create: HostFunction = &(jc3_fmod_create_sound as fn(&mut Environment));
    patch_thumb_hook(env, CREATE_SOUND_ADDR, "__touchHLE_JC3CreateSound", create);

    let play: HostFunction = &(jc3_sei_play as fn(&mut Environment));
    patch_thumb_hook(env, SEI_PLAY_ADDR, "__touchHLE_JC3SeiPlay", play);

    let stop: HostFunction = &(jc3_sei_stop as fn(&mut Environment));
    patch_thumb_hook(env, SEI_STOP_ADDR, "__touchHLE_JC3SeiStop", stop);

    log!(
        "Installed JellyCar 3 audio hooks: music @ {:#x}, SFX (createSound @ {:#x}, \
         play @ {:#x}, stop @ {:#x})",
        PLAY_MUSIC_ADDR,
        CREATE_SOUND_ADDR,
        SEI_PLAY_ADDR,
        SEI_STOP_ADDR
    );
}

/// Overwrite a Thumb function entry with an 8-byte veneer that switches to the
/// ARM host trampoline WITHOUT touching LR (so `bx lr` returns to the caller):
///   ldr r3,[pc,#0] (0x4b00); bx r3 (0x4718); .word stub_addr
/// r3 is caller-clobbered under AAPCS; r0-r2 (the args our shims read) survive.
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
    log_dbg!("JC3 audio: hooked {:#x} -> {:#x}", addr, stub_addr);
}

// ----------------------------------------------------------------------------
// SFX
// ----------------------------------------------------------------------------

/// `FMOD::System::createSound(this, const char* name, FMOD_MODE mode,
/// FMOD_CREATESOUNDEXINFO* exinfo, Sound** sound)`.
/// r0=this, r1=name, r2=mode, r3=exinfo, [sp]=Sound**. We return a unique
/// zeroed handle (deref-safe, like the old dummy Sound) and remember which wav
/// it is, so playback can find the file later. Returns FMOD_OK.
fn jc3_fmod_create_sound(env: &mut Environment) {
    let name_ptr = env.cpu.regs()[1];
    let mode = env.cpu.regs()[2];
    let sp = env.cpu.regs()[Cpu::SP];

    let name = match env.mem.cstr_at_utf8(ConstPtr::<u8>::from_bits(name_ptr)) {
        Ok(s) => s.to_owned(),
        Err(_) => String::new(),
    };
    let loop_flag = (mode & FMOD_LOOP_BITS) != 0;

    // Allocate a unique zeroed handle (0x400 like the old shared dummy Sound, so
    // the game's occasional field reads on the Sound* stay safe).
    let handle = env.mem.alloc(0x400);
    let hbits = handle.to_bits();
    for i in 0..0x100u32 {
        let p: MutPtr<u32> = Ptr::from_bits(hbits + i * 4);
        env.mem.write(p, 0u32);
    }

    if !name.is_empty() {
        sound_map().lock().unwrap().insert(hbits, (name, loop_flag));
    }

    // Write handle to *sound (the 5th arg, a Sound** on the stack).
    let out_slot: u32 = env.mem.read(ConstPtr::<u32>::from_bits(sp));
    if out_slot != 0 {
        let p: MutPtr<u32> = Ptr::from_bits(out_slot);
        env.mem.write(p, hbits);
    }
    env.cpu.regs_mut()[0] = 0; // FMOD_OK
}

/// `Walaber::SoundEffectInstance::play(float vol)`. r0=this, r1=vol (float bits,
/// softfp). The instance's `FMOD::Sound*` (our handle) is at [this+4].
fn jc3_sei_play(env: &mut Environment) {
    let this = env.cpu.regs()[0];
    if this == 0 {
        return;
    }
    let mut vol = f32::from_bits(env.cpu.regs()[1]);
    if !vol.is_finite() {
        vol = 1.0;
    }
    vol = vol.clamp(0.0, 1.0);

    let handle: u32 = env.mem.read(ConstPtr::<u32>::from_bits(this + 4));
    let entry = sound_map().lock().unwrap().get(&handle).cloned();
    let Some((name, loop_flag)) = entry else {
        return; // not a sound we created — leave it alone
    };

    if vol <= 0.0 {
        stop_instance(env, this);
        return;
    }

    // If this instance is already looping the same sound, leave it running
    // (avoids per-frame restart stutter for engine/tire/etc.).
    if loop_flag {
        let existing = instance_src().lock().unwrap().get(&this).copied();
        if let Some((src, _, h)) = existing {
            if h == handle && source_is_playing(env, src) {
                return;
            }
        }
    }

    let Some((pcm, format, rate)) = load_sfx(env, &name) else {
        return;
    };

    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    let new_src = unsafe {
        let mut map = instance_src().lock().unwrap();
        // Reap finished one-shots so we don't leak sources.
        let mut finished: Vec<u32> = Vec::new();
        for (&inst, &(src, buf, _)) in map.iter() {
            let mut st: ALint = 0;
            context.GetSourcei(src, AL_SOURCE_STATE, &mut st);
            if st != AL_PLAYING {
                context.DeleteSources(1, &src);
                context.DeleteBuffers(1, &buf);
                finished.push(inst);
            }
        }
        for inst in finished {
            map.remove(&inst);
        }
        // Replace any existing source for this exact instance.
        if let Some((src, buf, _)) = map.remove(&this) {
            context.SourceStop(src);
            context.DeleteSources(1, &src);
            context.DeleteBuffers(1, &buf);
        }
        let mut buffer: ALuint = 0;
        context.GenBuffers(1, &mut buffer);
        context.BufferData(
            buffer,
            format,
            pcm.as_ptr() as *const ALvoid,
            pcm.len() as ALsizei,
            rate,
        );
        let mut source: ALuint = 0;
        context.GenSources(1, &mut source);
        context.Sourcei(source, al::AL_BUFFER, buffer as ALint);
        context.Sourcei(source, AL_LOOPING, if loop_flag { 1 } else { 0 });
        context.Sourcef(source, AL_GAIN, vol);
        context.SourcePlay(source);
        map.insert(this, (source, buffer, handle));
        source
    };
    log_dbg!("JC3 SFX: play {} (inst {:#x}) -> AL src {}", name, this, new_src);
}

/// `Walaber::SoundEffectInstance::stop()`. r0=this.
fn jc3_sei_stop(env: &mut Environment) {
    let this = env.cpu.regs()[0];
    if this != 0 {
        stop_instance(env, this);
    }
}

fn stop_instance(env: &mut Environment, this: u32) {
    let entry = instance_src().lock().unwrap().remove(&this);
    let Some((src, buf, _)) = entry else {
        return;
    };
    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    unsafe {
        context.SourceStop(src);
        context.DeleteSources(1, &src);
        context.DeleteBuffers(1, &buf);
    }
}

fn source_is_playing(env: &mut Environment, src: ALuint) -> bool {
    let context = env
        .framework_state
        .audio_toolbox
        .make_al_context_current(env.openal_manager.as_mut());
    let mut st: ALint = 0;
    unsafe { context.GetSourcei(src, AL_SOURCE_STATE, &mut st) };
    st == AL_PLAYING
}

/// Resolve a createSound path (e.g. "Audio/Impact/hit1.wav" or a full guest
/// path) and decode it to interleaved 16-bit PCM.
fn load_sfx(env: &Environment, name: &str) -> Option<(Vec<u8>, ALenum, ALsizei)> {
    let bundle = env.bundle.bundle_path().as_str().to_owned();
    let candidates = [
        name.to_owned(),                        // already absolute?
        format!("{}/Content/{}", bundle, name), // "Audio/..." -> Content/Audio/...
        format!("{}/{}", bundle, name),
    ];
    for cand in candidates.iter() {
        let path = GuestPathBuf::from(cand.clone());
        if env.fs.is_file(&path) {
            return load_pcm(env, &path);
        }
    }
    log_dbg!("JC3 SFX: could not locate wav for {:?}", name);
    None
}

// ----------------------------------------------------------------------------
// Music (unchanged)
// ----------------------------------------------------------------------------

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

fn load_pcm(env: &Environment, path: &GuestPathBuf) -> Option<(Vec<u8>, ALenum, ALsizei)> {
    let mut file = match AudioFile::open_for_reading(path, &env.fs) {
        Ok(f) => f,
        Err(e) => {
            log_dbg!("JC3 audio: could not open {:?}: {:?}", path.as_str(), e);
            return None;
        }
    };
    let desc = file.audio_description();
    let byte_count = file.byte_count() as usize;
    let mut pcm = vec![0u8; byte_count];
    let read = match file.read_bytes(0, &mut pcm) {
        Ok(n) => n,
        Err(()) => {
            log_dbg!("JC3 audio: decode failed for {:?}", path.as_str());
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
