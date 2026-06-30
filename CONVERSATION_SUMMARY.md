# Full Conversation Summary — iOS 32-bit Emulation Project
## Date: June 30, 2026

---

## PURPOSE OF THIS FILE

This file exists because the AI conversation that produced all the progress below has a context limit and will expire. When you (a fresh AI with zero context) read this, you are picking up a complex multi-session project mid-stream. This file IS your context. Read it completely before doing anything.

**Your situation:** The user will paste this file or point you to it at the start of a new conversation. Your job is to continue exactly where we left off — specifically, getting JellyCar 3 rendering (it's one gate-bypass away from working).

**YOUR ENVIRONMENT:** You are running in the SAME Snowflake Cortex Code workspace as the previous AI. The entire repo is already at `/workspace/` — the touchHLE source, the IPA files, everything. You can read/edit/grep any file directly. You do NOT have a Rust compiler (`cargo`) — the user builds on their Kali Linux WSL2 machine after pulling your changes via `git pull`.

**CRITICAL FIRST STEP:** Before making ANY code changes, you MUST:
1. Read this ENTIRE file
2. Audit the following repo folders/files (they're ALL in your environment at `/workspace/touchHLE/`):
   - `src/environment.rs` (app startup + JC3 binary patch at line ~492)
   - `src/frameworks/core_animation/ca_display_link.rs` (one-shot layout trigger)
   - `src/frameworks/opengles/eagl.rs` (GLES2→GLES1 context fix)
   - `src/frameworks/opengles/gles_guest.rs` (atlas texcoord fix — **LOCKED, DO NOT TOUCH**)
   - `src/libc/unwind_sjlj.rs` (ALL shims: std::string, sqlite3, append, reserve)
   - `src/frameworks/uikit/ui_application.rs` (deferred didBecomeActive)
   - `src/frameworks/uikit/ui_view.rs` (views vec visibility)
   - `src/frameworks/uikit/ui_view_controller.rs` (navigationController stub)
   - `src/frameworks/uikit/ui_nib.rs` (UIClassSwapper + diagnostic log)
   - `src/objc/messages.rs` (lenient garbage-receiver guards)
   - `src/objc/objects.rs` (borrow/borrow_mut with msg ring dump)
   - `src/fs.rs` (CWD = bundle dir)
   - `src/window.rs` (iOS present_frame_to_calayer 180° rotation)
   - `src/frameworks/foundation/ns_property_list_serialization.rs` (XML + NSMutableData)
   - `src/frameworks/foundation/ns_string.rs` (initWithBytesNoCopy)
   - `src/frameworks/foundation/ns_file_manager.rs` (fileExistsAtPath guard)
   - `.github/workflows/` (iOS IPA builder — check before structural changes to avoid breaking the build)
3. Also in your workspace at `/workspace/`:
   - `JellyCar 1.5 (Decrypted).ipa` — JellyCar 1 (working, regression gate)
   - `JellyCar3 1.0 (Decrypted).ipa` — JellyCar 3 (the one we're fixing)
   - `Paper_Toss_World_Tour__Backflip_Studios___v1.40.6_LP_os50_-Widow.rc316.ipa` — PTWT (working, locked)
   - `CONVERSATION_SUMMARY.md` — THIS FILE
   - `README.md`
4. Broadly audit the touchHLE source tree (`src/`, `Cargo.toml`, `touchHLE_dylibs/`, `vendor/`) to understand the architecture before making changes. You don't need to read every file, but understand the module structure so you don't accidentally break something.
5. ONLY THEN proceed to fix JellyCar 3

---

## RED WIRES — DO NOT TOUCH THESE

These are working, locked, and proven. Modifying them WILL break games that currently work:

| File | What's locked | Why |
|------|---------------|-----|
| `src/frameworks/opengles/gles_guest.rs` | The `atlas_texcoord_offset()` function, `LAST_TEXCOORD_PTR`, the force-REPEAT logic, and the texture-matrix adjustment in `glDrawArrays`/`glDrawElements` | Fixes PTWT's photo thumbnails + level textures. Any change garbles rendering. |
| `src/libc/unwind_sjlj.rs` — the `_ZNSsC1EPKcRKSaIcE` / `_ZNSsC2EPKcRKSaIcE` shims | The NULL-tolerant std::string constructors | Prevents PTWT's fatal `std::logic_error` crash. Removing = instant abort. |
| `src/fs.rs` — `working_directory = home_directory.join(&bundle_dir_name)` | CWD = bundle directory at launch | PTWT and JC3 both rely on this to find bundled files via relative paths. |
| `src/window.rs` — the `upside_down` branch in `present_frame_to_calayer` | iOS-only 180° rotation for PortraitUpsideDown apps | PTWT displays right-side up on iOS because of this. |
| `src/environment.rs` — the orientation selection logic (PortraitUpsideDown path) | Keeps PTWT as PortraitUpsideDown orientation | Changing to Portrait causes iOS splash hang. |
| `src/objc/messages.rs` — the nil-isa and untracked-class guards | Lenient message dispatch for garbage receivers | PTWT crashes without these (CG text rendering path produces garbage pointers). |
| `src/frameworks/opengles/eagl.rs` — the `initWithAPI:` GLES2→GLES1 fallback | Accepts GLES2 requests, provides GLES1 context | JC3's EAGLView requests API 2; returning nil = no rendering ever. |

**If you modify ANY of the above, you MUST verify JellyCar 1, JellyCar 2, and Paper Toss World Tour still work.** The user will test on their Kali WSL2 machine. Ask them to run all three before declaring success.

---

## Project Overview

We are running legacy 32-bit iOS games on **touchHLE** (a high-level emulator for iPhone OS apps) — both on **desktop** (Kali Linux WSL2) for fast iteration/debugging, and on a **jailbroken iPad Mini 6 (iOS 16.1.1)** via a custom iOS port. The user's ultimate goal is a bounty: get specific games fully playable on the iOS port with graphics, audio, and touch input.

---

## Environment

### Workspace
- **Snowflake Cortex Code workspace** (cloud IDE) at `USER$.PUBLIC."IOS-binary-32-bit-monorepo-for-32bit-compatibility."`
- Files sync to GitHub repo: `BugTracker-BH/IOS-binary-32-bit-monorepo-for-32bit-compatibility.`
- The user pulls changes on their local machines via `git pull`

### Repo Structure
```
/workspace/
├── touchHLE/                    # Full touchHLE source (Rust + C++ deps)
│   ├── src/                     # Main Rust source
│   │   ├── libc/                # C library implementations
│   │   │   ├── unwind_sjlj.rs   # C++ exception handling + std::string shim + sqlite shim
│   │   │   ├── stdlib.rs         # _NSGetExecutablePath, getenv, etc.
│   │   │   ├── posix_io.rs       # File I/O with path logging
│   │   │   └── ...
│   │   ├── frameworks/          # Apple framework implementations
│   │   │   ├── foundation/      # NSString, NSKeyedUnarchiver, NSPropertyListSerialization...
│   │   │   ├── uikit/           # UIViewController, UITouch, UIView...
│   │   │   ├── opengles/        # GLES guest wrappers (gles_guest.rs, eagl.rs)
│   │   │   ├── core_animation/  # Composition, CAEAGLLayer, CADisplayLink
│   │   │   └── ...
│   │   ├── gles/                # GLES backend (gles1_on_gl2.rs, gles1_native.rs)
│   │   ├── objc/                # ObjC runtime (messages.rs, objects.rs, classes)
│   │   ├── environment.rs       # App startup, orientation, env vars, JC3 binary patch
│   │   ├── fs.rs                # Guest filesystem (CWD, bundle mounting)
│   │   ├── window.rs            # SDL window, input events, present_frame_to_calayer
│   │   ├── bundle.rs            # App bundle parsing
│   │   └── ...
│   ├── touchHLE_dylibs/         # Bundled guest ARM dylibs (libstdc++, libsqlite3, etc.)
│   ├── vendor/                  # C++ dependencies (dynarmic CPU emulator)
│   ├── Cargo.toml
│   └── ...
├── .github/workflows/           # GitHub Actions for iOS IPA builds
├── LiveExec32/                  # Unused, can be deleted
├── JellyCar 1.5 (Decrypted).ipa
├── JellyCar3 1.0 (Decrypted).ipa
├── Paper_Toss_World_Tour__Backflip_Studios___v1.40.6_LP_os50_-Widow.rc316.ipa
├── CONVERSATION_SUMMARY.md      # THIS FILE
└── README.md
```

### Testing Machines
1. **Kali Linux (WSL2)** — user's desktop, used for fast cargo builds + debugging
   - `cd ~/IOS-binary-32-bit-monorepo-for-32bit-compatibility./touchHLE`
   - `export CMAKE_POLICY_VERSION_MINIMUM=3.5` (needed if CMake cache cleared)
   - `ALSOFT_DRIVERS=null cargo run -- "../<game>.ipa" 2>&1 | tee <log>.log`
   - Uses llvmpipe (software OpenGL 4.5) — no native GLES1 available
   - Mouse input maps to single-finger touch

2. **iPad Mini 6 (2021, WiFi, jailbroken iOS 16.1.1)** — final target
   - SSH: `ssh root@192.168.1.4` (password auth)
   - IPA built via GitHub Actions workflow, installed on device
   - Real touch input (multi-finger SDL finger events)
   - touchHLE_log.txt at: `find /var/mobile -name touchHLE_log.txt`
   - Present path: `present_frame_to_calayer` (iOS-only, CPU pixel rotation)

3. **iPhone 15 Pro Max (iOS 18.3)** — secondary test device (non-jailbroken)

### Build Notes
- Rust toolchain NOT available in the Cortex Code sandbox — all edits are blind (no `cargo check`)
- User builds on Kali (WSL2) for desktop testing
- iOS IPA builds happen via GitHub Actions workflow triggered by push to main
- CMake 4.x on Kali needs `CMAKE_POLICY_VERSION_MINIMUM=3.5` env var for dynarmic

---

## Methodology

### Debugging Workflow
1. **Run the game** on WSL2 with logging → capture crash/panic line
2. **Identify the root cause** from the log (panic location, stack trace, warnings)
3. **Make targeted edits** in the Cortex Code workspace
4. **User pulls + rebuilds** on Kali (`git pull && cargo run`)
5. **Iterate** until the crash clears, then move to the next one
6. **iOS testing** only after desktop works — rebuild IPA via Actions, install, verify

### Key Principles
- **NEVER touch locked/working code** (PTWT atlas fix, std::string shim, orientation) without explicit permission
- **Don't guess blindly** — use diagnostics to confirm hypotheses before fixing
- **Test on WSL2 first** (fast iteration), iOS last (slow rebuild cycle)
- **JellyCar 1 & 2 are the regression gate** — any change must not break them
- **The atlas-texcoord fix in gles_guest.rs is LOCKED** — do not modify

---

## Completed: Paper Toss World Tour — FULLY WORKING

The game was rated "completely broken: crashes immediately" in the touchHLE compatibility database. We took it from that state to fully playable with correct rendering in a single session.

### Fixes Applied:
| Fix | File | Description |
|-----|------|-------------|
| CWD = bundle dir | `src/fs.rs` | iPhone OS apps launch with CWD = .app bundle; was hardcoded to "/" |
| `_NSGetExecutablePath` | `src/libc/stdlib.rs` | Was a no-op stub returning 0; now writes the real bundle path |
| `std::string(NULL)` shim | `src/libc/unwind_sjlj.rs` | Intercepts libstdc++ string constructors; substitutes "" for NULL |
| `__cxa_throw` diagnostics | `src/libc/unwind_sjlj.rs` | Reports thrown C++ type + guest stack when no SjLj handler found |
| Plist XML output + error param | `src/frameworks/foundation/ns_property_list_serialization.rs` | Added XML format + non-null error param + NSMutableData subclass |
| `initWithBytesNoCopy:...` | `src/frameworks/foundation/ns_string.rs` | New NSString initializer |
| `navigationController` / `parentViewController` | `src/frameworks/uikit/ui_view_controller.rs` | Returns nil |
| `applicationState` | `src/frameworks/uikit/ui_application.rs` | Returns UIApplicationStateActive |
| objc_msgSend lenient guards | `src/objc/messages.rs` | Handles garbage receivers → returns nil |
| `open()` path logging | `src/libc/posix_io.rs` | Logs actual path string on failure |
| `fileExistsAtPath:` guard | `src/frameworks/foundation/ns_file_manager.rs` | Returns false for untracked path objects |
| Atlas texcoord origin alignment | `src/frameworks/opengles/gles_guest.rs` | **LOCKED** — texture-matrix translation for far-out-of-range coords |
| iOS present 180° rotation | `src/window.rs` | `present_frame_to_calayer` rotates for PortraitUpsideDown |
| Deferred didBecomeActive | `src/frameworks/uikit/ui_application.rs` | Re-delivers lifecycle after VC setup |
| Message ring dump on borrow panic | `src/objc/objects.rs` | Dumps recent ObjC messages before crash |
| `enumerateObjectsUsingBlock:` | (not fully implemented — was in progress) | |

### Known Remaining Issues (PTWT, cosmetic):
- **Stats bar garble** ("Submit / Score / Best / Menu") — atlas-texcoord fix that works for photos/levels also shifts UI glyphs. Cannot fix without breaking levels. LEAVE IT.
- **iOS touch/navigation bug** — back buttons stop responding after page transitions on iOS only (works with mouse on WSL2). Diagnostics staged (`[nav-touch]` logs) but not yet captured on device.

---

## Completed: JellyCar 1 & 2 — FLAWLESS

Pre-existing support, maintained as the regression gate. Landscape mode, GLES1 fixed-function. All our changes are gated to not affect their rendering or input.

---

## IN PROGRESS: JellyCar 3

### Game Identity
- **File:** `JellyCar3 1.0 (Decrypted).ipa`
- **Bundle ID:** `com.disney.JellyCar3`
- **Binary:** FAT (armv6 + armv7), touchHLE loads the armv7 slice
- **Architecture:** 32-bit ARM — touchHLE compatible ✅
- **Renderer:** Despite shipping Shader.fsh/vsh, it uses **GLES1 fixed-function** at runtime ✅
- **Orientation:** Landscape (same as JC1/2) ✅
- **Min OS:** 3.1.3
- **Engine:** Walaber (same as JC1/JC2 which work perfectly)

### What Works (confirmed by logs):
1. **Binary loads and runs** — armv7 slice, all static initializers execute
2. **GLES1 context creates successfully** — game requests API 2 (GLES2), our fix gives GLES1 context. TWO contexts created (confirmed by "Driver info" lines in log)
3. **EAGLView instantiated from nib** — UIClassSwapper fired for "EAGLView" (confirmed in log)
4. **CADisplayLink fires every frame** — presents #0, #1, #2... appear continuously (1800+ in one session)
5. **SQLite queries succeed** — "Column" keyword fix rewrites queries correctly
6. **std::string crash bypassed** — append shim catches corrupted FMOD strings
7. **Touch input registers** — `[nav-touch]` lines show touches being delivered to views
8. **No crashes** — the game runs indefinitely without abort/panic

### What's Blocking (THE one remaining issue):
The game's `drawFrame` method (at vaddr `0xbfda0`) runs every frame but **skips all GL draw calls**. It checks an internal flag (a byte loaded from `self + ivar_offset`) and if the flag is 0, it early-returns after presenting an empty framebuffer. The flag is 0 because FMOD audio failed to initialize, and the game's state machine never transitions to "ready to render."

**Concrete evidence:**
- `glColorPointer` calls appeared in earlier runs during INIT (22 calls from `initNewGame`) but NOT from the display-link-driven `drawFrame` path
- Zero `glDrawArrays`, `glDrawElements`, or `glClear` calls in any run
- The gate is at vaddr `0xbfe0c`: `cbnz r3, #0xbfe20` (if flag != 0, render; else skip)
- A runtime binary patch was attempted (replace `cbnz` with unconditional `b`) but didn't take effect despite cache invalidation

### Fixes Already Applied for JC3:
| Fix | File | Description |
|-----|------|-------------|
| SQLite "Column" keyword | `src/libc/unwind_sjlj.rs` | Rewrites `Column=` → `"Column"=` in sqlite3_prepare_v2 |
| std::string::append guard | `src/libc/unwind_sjlj.rs` | Skips append if source string is corrupted (NULL data or absurd length) |
| std::string::reserve guard | `src/libc/unwind_sjlj.rs` | Clamps reserve > 256MB to prevent length_error throw |
| Accept GLES2 context requests | `src/frameworks/opengles/eagl.rs` | Returns GLES1 context instead of nil for API 2/3 requests |
| Deferred didBecomeActive | `src/frameworks/uikit/ui_application.rs` | Fires applicationDidBecomeActive: on next run loop via performSelector:afterDelay: |
| One-shot layoutSubviews | `src/frameworks/core_animation/ca_display_link.rs` | On first display link fire, calls layoutSubviews on target VC's view + subviews |
| UIClassSwapper logging | `src/frameworks/uikit/ui_nib.rs` | Logs className/originalClassName during nib decode |
| views vec visibility | `src/frameworks/uikit/ui_view.rs` | Changed from pub(super) to pub(crate) |
| Runtime binary patch | `src/environment.rs` | Patches 0xbfe0c with unconditional branch (NOT YET WORKING) |

---

## WHAT THE FRESH AI SHOULD DO FIRST

### Step 0: Audit before touching anything
1. Read THIS file completely
2. Read `src/environment.rs` lines 482-500 (the JC3 binary patch)
3. Read `src/frameworks/core_animation/ca_display_link.rs` (the one-shot layout)
4. Read `src/frameworks/opengles/eagl.rs` lines 154-180 (GLES2→GLES1 fix)
5. Read `src/libc/unwind_sjlj.rs` (ALL the shims — string, sqlite, append, reserve)
6. Read `src/frameworks/opengles/gles_guest.rs` around `atlas_texcoord_offset` (LOCKED, don't touch)
7. Confirm JellyCar 1 still works: `ALSOFT_DRIVERS=null cargo run -- "../JellyCar 1.5 (Decrypted).ipa"`

### Step 1: Fix the render gate bypass
The binary patch at `0xbfe0c` was applied and logged but had no effect. Possible reasons:
1. **The game's `drawFrame` address changes between runs** — the FAT binary armv7 slice loads at __TEXT vmaddr `0x1000` (confirmed), `drawFrame` symbol is at `0xbfda0`. Verify by checking if the patch bytes actually changed in memory (read back after write).
2. **Cache invalidation timing** — the patch runs before any guest code executes ("CPU emulation begins now" is line 222, before static initializers). The JIT shouldn't have compiled 0xbfe0c yet. But maybe dynarmic pre-scans the binary.
3. **Wrong gate** — the actual render-blocking condition might be in a DIFFERENT function called from drawFrame, not the `cbnz` at 0xbfe0c. The disassembly showed TWO gates: first at 0xbfdd0-0xbfdd4 (checks one flag), second at 0xbfe08-0xbfe0c (checks another). Maybe the FIRST gate is the real blocker.
4. **The `startAnimation` double-call created a second display link** that interfered (now removed, but the damage may persist in the save state).

**Recommended approach:**
- Add a **host hook at a known-to-execute address** (like the `presentRenderbuffer:` call inside drawFrame's return path — we know presents happen) that logs the CPU registers at that point. This confirms whether the patched code path is being taken.
- Alternatively, use GDB (`--gdb=localhost:9000`) to set a breakpoint at 0xbfe0c and inspect whether the instruction bytes are actually 0x08 0xE0 (the patch) or still the original `cbnz`.
- If the patch IS there but still not rendering, there's likely a THIRD gate deeper in the render function (after the two I found). Disassemble further into drawFrame (past 0xbfe20) to find it.

### Step 2: Alternative approach (if patching remains stubborn)
Instead of patching the gate, **find and set the flag directly**:
- The flag is at `self + ivar_offset` where `self` is the `JellyCar3ViewController` instance
- The ivar offset is loaded from a PC-relative constant at 0xbfe00-0xbfe04
- Read that constant to get the offset, find the VC instance (it's the CADisplayLink's target), compute the flag address, and write 1 to it from the one-shot code in ca_display_link.rs

### Step 3: After rendering works
Once GL draw calls appear, the game will likely hit more gaps (same as PTWT did after first frame):
- Missing textures (PVRTC decompression — touchHLE has this)
- Missing ObjC selectors
- Rendering artifacts
Follow the same PTWT methodology: one crash at a time, each fix getting deeper.

---

## Critical Technical Details

### The atlas texcoord fix (gles_guest.rs) — DO NOT TOUCH
`atlas_texcoord_offset()` reads the last-set texcoord pointer, finds min U/V, and if min < -2.0, returns a translation to align the quad to the texture origin. Applied via `glMatrixMode(GL_TEXTURE)` + `glTranslatef` before each draw, reset after. Combined with force-`GL_REPEAT`. This fixes PTWT's photos and levels. It does NOT fix the stats bar (entangled). **LEAVE IT ALONE.**

### The std::string shim pattern (reusable)
Host functions registered in `FUNCTIONS` take link precedence over guest dylib exports. The shim reads guest memory, optionally modifies data, then calls `resolve_guest_export(env, "_symbolname")` to get the real guest function and invokes it via `call_from_host`. No recursion because intra-dylib calls bypass import stubs.

### The sqlite3 shim pattern
Same as above but for sqlite3_prepare_v2. Reads the SQL string from guest memory, checks for "Column=" (SQLite keyword issue), rewrites it with proper quoting, allocates a temp guest buffer, calls the real sqlite3, frees the buffer.

### Build commands
```bash
cd ~/IOS-binary-32-bit-monorepo-for-32bit-compatibility./touchHLE
git pull
export CMAKE_POLICY_VERSION_MINIMUM=3.5
ALSOFT_DRIVERS=null cargo run -- "../<game>.ipa" 2>&1 | tee <log>.log
```

### iOS log retrieval
```bash
ssh root@192.168.1.4
cat "$(find /var/mobile -name touchHLE_log.txt 2>/dev/null | head -1)"
```

---

## Games Status

| File | Game | Status |
|------|------|--------|
| `JellyCar 1.5 (Decrypted).ipa` | JellyCar 1 (v1.5, Walaber, 2009) | ✅ Fully working |
| `Paper_Toss_World_Tour__...rc316.ipa` | Paper Toss World Tour (v1.40.6, Backflip, 2014) | ✅ Fully working |
| `JellyCar3 1.0 (Decrypted).ipa` | JellyCar 3 (v1.0, Disney/Walaber, 2011) | 🔧 One gate away from rendering |

---

## Why JC3 is 99% confirmed runnable (evidence)

1. **Same engine** as JC1/JC2 which render perfectly — Walaber's C++ game engine with GLES1 fixed-function rendering
2. **GL context creates and works** — two successful `Driver info: OpenGL 4.5` lines confirm the EAGL context is functional
3. **The rendering pipeline is proven** — when JC1's EAGLView was triggered (accidentally, during the addSubview experiment), it went through `layoutSubviews → createFramebuffer → renderbufferStorage → drawView` and reached actual GL commands before crashing on timing (PC=0x1000 from premature drawView call). That proves the ENTIRE GL pipeline works.
4. **The game's render code exists and is reachable** — `glColorPointer` calls appeared in init (22 of them), proving the Walaber rendering engine is executing GL commands. The only thing stopping it from doing so in the display-link path is that one flag check.
5. **Presents happen continuously** — the display link fires, the present path works, the framebuffer is being swapped. Only the CONTENT is missing because the draw calls are gated.
6. **No fundamental incompatibility** — armv7 ✅, GLES1 ✅, same frameworks ✅, no GLES2 shader calls ✅
