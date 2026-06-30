/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Abstraction of window setup, OpenGL context creation and event handling.
//!
//! Implemented using the sdl2 crate (a Rust wrapper for SDL2). All usage of
//! SDL should be confined to this module.
//!
//! There is currently no separation of concerns between a single window and
//! window system interaction in general, because it is assumed only one window
//! will be needed for the runtime of the app.

use crate::gles::present::present_frame;
use crate::gles::{create_gles1_ctx_no_parent_stack, GLESContext, GLES};
use crate::image::Image;
use crate::matrix::Matrix;
use crate::options::Options;
use crate::Environment;
use sdl2::mouse::MouseButton;
use sdl2::pixels::PixelFormatEnum;
use sdl2::surface::Surface;
use sdl2_sys::SDL_PowerState;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::f32::consts::{FRAC_PI_2, PI};
use std::num::NonZeroU32;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DeviceFamily {
    iPhone,
    iPad,
}
impl std::fmt::Display for DeviceFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}
impl DeviceFamily {
    pub fn portrait_size(&self) -> (u32, u32) {
        match self {
            DeviceFamily::iPhone => (320, 480),
            DeviceFamily::iPad => (768, 1024),
        }
    }
}
impl TryFrom<u64> for DeviceFamily {
    type Error = ();
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(DeviceFamily::iPhone),
            2 => Ok(DeviceFamily::iPad),
            _ => Err(()),
        }
    }
}
impl TryFrom<&str> for DeviceFamily {
    type Error = ();
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "iphone" => Ok(DeviceFamily::iPhone),
            "ipad" => Ok(DeviceFamily::iPad),
            _ => Err(()),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DeviceOrientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
}
fn size_for_orientation(
    family: DeviceFamily,
    orientation: DeviceOrientation,
    scale_hack: NonZeroU32,
) -> (u32, u32) {
    let (width, height) = family.portrait_size();
    let scale_hack = scale_hack.get();
    match orientation {
        DeviceOrientation::Portrait => (width * scale_hack, height * scale_hack),
        DeviceOrientation::PortraitUpsideDown => (width * scale_hack, height * scale_hack),
        DeviceOrientation::LandscapeLeft => (height * scale_hack, width * scale_hack),
        DeviceOrientation::LandscapeRight => (height * scale_hack, width * scale_hack),
    }
}
fn rotate_fullscreen_size(orientation: DeviceOrientation, screen_size: (u32, u32)) -> (u32, u32) {
    let (short_side, long_side) = if screen_size.0 < screen_size.1 {
        (screen_size.0, screen_size.1)
    } else {
        (screen_size.1, screen_size.0)
    };
    match orientation {
        DeviceOrientation::Portrait | DeviceOrientation::PortraitUpsideDown => {
            (short_side, long_side)
        }
        DeviceOrientation::LandscapeLeft | DeviceOrientation::LandscapeRight => {
            (long_side, short_side)
        }
    }
}

/// iOS-only: log the real UIKit orientation state (screen/window bounds, the
/// current interface orientation, and the root view controller's
/// `supportedInterfaceOrientations` mask + `shouldAutorotate`). This pinpoints
/// why the app isn't rotating to landscape: if SDL's VC mask doesn't include the
/// landscape bits, iOS will never rotate regardless of our hint. Must be called
/// on the main thread.
///
/// UIInterfaceOrientation: 1=Portrait 2=PortraitUpsideDown 3=LandscapeRight
/// 4=LandscapeLeft. Mask bits: Portrait=2 LandscapeRight=8 LandscapeLeft=16
/// (Landscape=24, AllButUpsideDown=26).
#[cfg(target_os = "ios")]
unsafe fn ios_dump_orientation(tag: &str) {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_void};
    type Id = *mut c_void;
    type Sel = *mut c_void;
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn object_getClassName(obj: Id) -> *const c_char;
    }
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct R {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe fn sel(s: &str) -> Sel {
        sel_registerName(CString::new(s).unwrap().as_ptr())
    }
    unsafe fn cls(s: &str) -> Id {
        objc_getClass(CString::new(s).unwrap().as_ptr())
    }
    unsafe fn msg0(o: Id, s: Sel) -> Id {
        if o.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn rectof(o: Id, s: Sel) -> R {
        if o.is_null() {
            return R { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
        }
        let f: extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn intof(o: Id, s: Sel) -> i64 {
        if o.is_null() {
            return -1;
        }
        let f: extern "C" fn(Id, Sel) -> i64 = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn boolof(o: Id, s: Sel) -> bool {
        if o.is_null() {
            return false;
        }
        let f: extern "C" fn(Id, Sel) -> bool = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn clsname(o: Id) -> String {
        if o.is_null() {
            return "nil".to_string();
        }
        let p = object_getClassName(o);
        if p.is_null() {
            return "?".to_string();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }

    let app = msg0(cls("UIApplication"), sel("sharedApplication"));
    let screen = msg0(cls("UIScreen"), sel("mainScreen"));
    let sb = rectof(screen, sel("bounds"));
    let mut key = msg0(app, sel("keyWindow"));
    if key.is_null() {
        let windows = msg0(app, sel("windows"));
        let count = intof(windows, sel("count"));
        if count > 0 {
            let f: extern "C" fn(Id, Sel, i64) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            key = f(windows, sel("objectAtIndex:"), 0);
        }
    }
    let wb = rectof(key, sel("bounds"));
    let status_orient = intof(app, sel("statusBarOrientation"));
    let root = msg0(key, sel("rootViewController"));
    let root_name = clsname(root);
    let supported = intof(root, sel("supportedInterfaceOrientations"));
    let autorotate = boolof(root, sel("shouldAutorotate"));

    log!(
        "[diag-orient] {}: screen={}x{} keyWindow={}x{} statusBarOrient={} rootVC={} supportedMask={} shouldAutorotate={}",
        tag, sb.w, sb.h, wb.w, wb.h, status_orient, root_name, supported, autorotate
    );
}

/// iOS-only: ask iOS to re-evaluate the app's interface orientation. Changing
/// SDL's view-controller's `supportedInterfaceOrientations` at runtime does NOT
/// auto-rotate the app — iOS only re-queries when explicitly told. Without this,
/// a landscape game's window stays portrait (letterboxed strip). Dispatched to
/// the main thread because all of this is UIKit.
#[cfg(target_os = "ios")]
fn ios_request_orientation_update() {
    use std::os::raw::c_void;
    extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }
    unsafe {
        dispatch_async_f(
            &_dispatch_main_q as *const c_void,
            std::ptr::null_mut(),
            ios_orientation_update_work,
        );
    }
}

#[cfg(target_os = "ios")]
extern "C" fn ios_orientation_update_work(_ctx: *mut std::os::raw::c_void) {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    type Id = *mut c_void;
    type Sel = *mut c_void;
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }
    unsafe fn sel(s: &str) -> Sel {
        sel_registerName(CString::new(s).unwrap().as_ptr())
    }
    unsafe fn cls(s: &str) -> Id {
        objc_getClass(CString::new(s).unwrap().as_ptr())
    }
    unsafe fn msg0(o: Id, s: Sel) -> Id {
        if o.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn ival(o: Id, s: Sel) -> i64 {
        if o.is_null() {
            return 0;
        }
        let f: extern "C" fn(Id, Sel) -> i64 = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn responds(o: Id, name: &str) -> bool {
        if o.is_null() {
            return false;
        }
        let f: extern "C" fn(Id, Sel, Sel) -> bool = std::mem::transmute(objc_msgSend as *const ());
        f(o, sel("respondsToSelector:"), sel(name))
    }

    unsafe {
        let app = msg0(cls("UIApplication"), sel("sharedApplication"));
        let mut key = msg0(app, sel("keyWindow"));
        if key.is_null() {
            let windows = msg0(app, sel("windows"));
            let count = ival(windows, sel("count"));
            if count > 0 {
                let f: extern "C" fn(Id, Sel, i64) -> Id =
                    std::mem::transmute(objc_msgSend as *const ());
                key = f(windows, sel("objectAtIndex:"), 0);
            }
        }
        let root = msg0(key, sel("rootViewController"));

        // iOS 16+: tell the VC its supported orientations changed (modern
        // replacement for attemptRotationToDeviceOrientation).
        if responds(root, "setNeedsUpdateOfSupportedInterfaceOrientations") {
            msg0(root, sel("setNeedsUpdateOfSupportedInterfaceOrientations"));
        }

        // iOS 16+: explicitly request the window scene rotate to the VC's
        // currently-supported orientation(s).
        let scene = msg0(key, sel("windowScene"));
        let mask = ival(root, sel("supportedInterfaceOrientations")) as u64;
        let scene_responds =
            !scene.is_null() && responds(scene, "requestGeometryUpdateWithPreferences:errorHandler:");
        eprintln!(
            "[ios] orient-update: root_nil={} scene_nil={} mask={} scene_responds_geom={} setNeedsUpdate_responds={} attemptRotation_responds={}",
            root.is_null(),
            scene.is_null(),
            mask,
            scene_responds,
            responds(root, "setNeedsUpdateOfSupportedInterfaceOrientations"),
            responds(cls("UIViewController"), "attemptRotationToDeviceOrientation"),
        );
        if scene_responds {
            let prefs_cls = cls("UIWindowSceneGeometryPreferencesIOS");
            if !prefs_cls.is_null() {
                let prefs = msg0(prefs_cls, sel("alloc"));
                let prefs = {
                    let f: extern "C" fn(Id, Sel, u64) -> Id =
                        std::mem::transmute(objc_msgSend as *const ());
                    f(prefs, sel("initWithInterfaceOrientations:"), mask)
                };
                if !prefs.is_null() {
                    let f: extern "C" fn(Id, Sel, Id, Id) =
                        std::mem::transmute(objc_msgSend as *const ());
                    f(
                        scene,
                        sel("requestGeometryUpdateWithPreferences:errorHandler:"),
                        prefs,
                        std::ptr::null_mut(),
                    );
                    eprintln!("[ios] orient-update: requestGeometryUpdate CALLED mask={}", mask);
                } else {
                    eprintln!("[ios] orient-update: prefs alloc/init FAILED");
                }
            } else {
                eprintln!("[ios] orient-update: UIWindowSceneGeometryPreferencesIOS class MISSING");
            }
        }

        // Legacy fallback (iOS < 16): force a rotation re-evaluation.
        let vc_cls = cls("UIViewController");
        if responds(vc_cls, "attemptRotationToDeviceOrientation") {
            msg0(vc_cls, sel("attemptRotationToDeviceOrientation"));
        }
        eprintln!("[ios] requested orientation update");
    }
}

/// iOS-only: introspect the live UIKit view hierarchy via the Objective-C
/// runtime and log it, to diagnose why SDL's GL view (a CAEAGLLayer) is not
/// being composited to the screen. Must be called on the main thread.
#[cfg(target_os = "ios")]
unsafe fn ios_dump_view_hierarchy(tag: &str) {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    type Id = *mut c_void;
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
        fn object_getClassName(obj: Id) -> *const c_char;
    }
    unsafe fn sel(name: &str) -> *mut c_void {
        sel_registerName(CString::new(name).unwrap().as_ptr())
    }
    unsafe fn class(name: &str) -> Id {
        objc_getClass(CString::new(name).unwrap().as_ptr())
    }
    // id obj.selector()  (no args)
    unsafe fn msg(obj: Id, selector: &str) -> Id {
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, *mut c_void) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, sel(selector))
    }
    // BOOL obj.selector()  (returned as i8 to avoid bool-ABI UB)
    unsafe fn msg_i8(obj: Id, selector: &str) -> i8 {
        if obj.is_null() {
            return -1;
        }
        let f: extern "C" fn(Id, *mut c_void) -> i8 =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, sel(selector))
    }
    // NSUInteger obj.selector()
    unsafe fn msg_uint(obj: Id, selector: &str) -> usize {
        if obj.is_null() {
            return 0;
        }
        let f: extern "C" fn(Id, *mut c_void) -> usize =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, sel(selector))
    }
    // CGFloat obj.selector()
    unsafe fn msg_f64(obj: Id, selector: &str) -> f64 {
        if obj.is_null() {
            return f64::NAN;
        }
        let f: extern "C" fn(Id, *mut c_void) -> f64 =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, sel(selector))
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RectRaw {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    // CGRect obj.selector()  (arm64 returns the 32-byte struct via x8/sret)
    unsafe fn msg_rect(obj: Id, selector: &str) -> RectRaw {
        if obj.is_null() {
            return RectRaw { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
        }
        let f: extern "C" fn(Id, *mut c_void) -> RectRaw =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, sel(selector))
    }
    unsafe fn at_index(arr: Id, i: usize) -> Id {
        if arr.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, *mut c_void, usize) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(arr, sel("objectAtIndex:"), i)
    }
    unsafe fn cls_name(obj: Id) -> String {
        if obj.is_null() {
            return "(nil)".to_string();
        }
        let p = object_getClassName(obj);
        if p.is_null() {
            return "(?)".to_string();
        }
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
    // Recursively log a UIView subtree: class, frame, hidden, alpha, opaque, and
    // whether its layer has CGImage contents. An opaque non-hidden view that
    // covers the GL view is exactly what would produce a black screen while our
    // framebuffer readback shows correct pixels.
    unsafe fn dump_view(view: Id, depth: usize, tag: &str) {
        if view.is_null() || depth > 8 {
            return;
        }
        let name = cls_name(view);
        let frame = msg_rect(view, "frame");
        let hidden = msg_i8(view, "isHidden");
        let alpha = msg_f64(view, "alpha");
        let opaque = msg_i8(view, "isOpaque");
        let layer = msg(view, "layer");
        let contents = msg(layer, "contents");
        let bg = msg(view, "backgroundColor");
        let indent = "  ".repeat(depth);
        log!(
            "[diag-tree] {} {}{} frame=({:.0},{:.0},{:.0},{:.0}) hidden={} alpha={:.2} opaque={} layer={:?} contents={:?} bg={:?}",
            tag, indent, name, frame.x, frame.y, frame.w, frame.h, hidden, alpha, opaque, layer, contents, bg
        );
        let subs = msg(view, "subviews");
        let n = msg_uint(subs, "count");
        for i in 0..n {
            dump_view(at_index(subs, i), depth + 1, tag);
        }
    }

    let app = msg(class("UIApplication"), "sharedApplication");
    let mut key_window = msg(app, "keyWindow");
    if key_window.is_null() {
        // keyWindow is deprecated on iOS 13+ and may be nil; fall back to
        // windows[0].
        let windows = msg(app, "windows");
        let count = msg_uint(windows, "count");
        if count > 0 {
            let f: extern "C" fn(Id, *mut c_void, usize) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            key_window = f(windows, sel("objectAtIndex:"), 0);
        }
        log!("[diag-objc] {}: keyWindow was nil; windows.count={}", tag, count);
    }
    let root_vc = msg(key_window, "rootViewController");
    let root_view = msg(root_vc, "view");
    let root_view_window = msg(root_view, "window");
    let subviews = msg(root_view, "subviews");
    let subview_count = msg_uint(subviews, "count");
    let is_key = msg_i8(key_window, "isKeyWindow");
    let hidden = msg_i8(key_window, "isHidden");
    log!(
        "[diag-objc] {}: keyWindow={:?} isKey={} hidden={} rootVC={:?} rootView={:?} rootView.window={:?} rootView.subviews={}",
        tag, key_window, is_key, hidden, root_vc, root_view, root_view_window, subview_count
    );
    // Full recursive dump of the key window's view subtree. Look for an opaque,
    // non-hidden view drawn ON TOP of (i.e. after) SDL's GL view that would hide
    // the GL output.
    dump_view(key_window, 0, tag);
}

// ============================================================================
// iOS native CoreAnimation presenter.
//
// On iOS the OpenGL ES / EAGL `presentRenderbuffer:` path does NOT reach the
// display (confirmed on device + simulator: the presented renderbuffer holds
// the correct frame, the CAEAGLLayer is visible with contents, yet the screen
// stays black). OpenGL ES is deprecated on iOS and SDL's CAEAGLLayer present is
// not being composited by the render server.
//
// Instead we present the finished frame through plain CoreAnimation, which is
// guaranteed to composite: each frame we wrap the composited pixels in a
// CGImage and assign it to the `contents` of an overlay CALayer placed on top
// of the window. The contents update is marshalled to the main thread.
// ============================================================================

#[cfg(target_os = "ios")]
static OVERLAY_LAYER: std::sync::atomic::AtomicPtr<std::os::raw::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// The window layer the overlay is currently attached to. If the key window
/// changes (e.g. the app-picker window is torn down and the game's window is
/// created), the overlay must be moved to the new window or the new window
/// renders black (the classic "second window" bug).
#[cfg(target_os = "ios")]
static OVERLAY_WINDOW_LAYER: std::sync::atomic::AtomicPtr<std::os::raw::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(target_os = "ios")]
#[repr(C)]
struct PresentPayload {
    /// malloc'd RGBA8 buffer, top-left origin (already vertically flipped).
    buf: *mut std::os::raw::c_void,
    w: usize,
    h: usize,
}

/// CGDataProvider release callback: frees the malloc'd pixel buffer once the
/// CGImage (and thus the layer contents) that owns it is released.
#[cfg(target_os = "ios")]
extern "C" fn present_dataprovider_release(
    _info: *mut std::os::raw::c_void,
    data: *const std::os::raw::c_void,
    _size: usize,
) {
    unsafe { libc::free(data as *mut std::os::raw::c_void) };
}

/// Runs on the main thread (via dispatch_async_f). Builds a CGImage from the
/// payload and assigns it to the overlay layer's contents, creating the overlay
/// layer on first use.
#[cfg(target_os = "ios")]
extern "C" fn present_on_main(ctx: *mut std::os::raw::c_void) {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    use std::sync::atomic::Ordering;
    type Id = *mut c_void;

    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
        fn CGColorSpaceCreateDeviceRGB() -> Id;
        fn CGColorSpaceRelease(space: Id);
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const c_void,
            size: usize,
            release: extern "C" fn(*mut c_void, *const c_void, usize),
        ) -> Id;
        fn CGDataProviderRelease(p: Id);
        fn CGImageCreate(
            width: usize,
            height: usize,
            bits_per_component: usize,
            bits_per_pixel: usize,
            bytes_per_row: usize,
            space: Id,
            bitmap_info: u32,
            provider: Id,
            decode: *const f64,
            should_interpolate: bool,
            intent: i32,
        ) -> Id;
        fn CGImageRelease(img: Id);
        static kCAGravityResizeAspect: Id;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RectRaw {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    unsafe fn sel(name: &str) -> *mut c_void {
        sel_registerName(CString::new(name).unwrap().as_ptr())
    }
    unsafe fn cls(name: &str) -> Id {
        objc_getClass(CString::new(name).unwrap().as_ptr())
    }
    // obj.selector()
    unsafe fn msg0(obj: Id, selector: *mut c_void) -> Id {
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, *mut c_void) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, selector)
    }
    // obj.selector(arg) where arg is an object pointer
    unsafe fn msg1(obj: Id, selector: *mut c_void, arg: Id) -> Id {
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, *mut c_void, Id) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(obj, selector, arg)
    }

    if ctx.is_null() {
        return;
    }
    let payload = unsafe { Box::from_raw(ctx as *mut PresentPayload) };
    if payload.buf.is_null() {
        return;
    }

    unsafe {
        // Build the CGImage. The provider takes ownership of payload.buf and
        // frees it via present_dataprovider_release when released.
        let space = CGColorSpaceCreateDeviceRGB();
        let provider = CGDataProviderCreateWithData(
            std::ptr::null_mut(),
            payload.buf,
            payload.w * payload.h * 4,
            present_dataprovider_release,
        );
        const KCG_ALPHA_NONE_SKIP_LAST: u32 = 5; // kCGImageAlphaNoneSkipLast
        let img = CGImageCreate(
            payload.w,
            payload.h,
            8,
            32,
            payload.w * 4,
            space,
            KCG_ALPHA_NONE_SKIP_LAST,
            provider,
            std::ptr::null(),
            false,
            0,
        );

        // Locate the key window's root layer.
        let app = msg0(cls("UIApplication"), sel("sharedApplication"));
        let mut key_window = msg0(app, sel("keyWindow"));
        if key_window.is_null() {
            let windows = msg0(app, sel("windows"));
            let count: usize = {
                let f: extern "C" fn(Id, *mut c_void) -> usize =
                    std::mem::transmute(objc_msgSend as *const ());
                f(windows, sel("count"))
            };
            if count > 0 {
                let f: extern "C" fn(Id, *mut c_void, usize) -> Id =
                    std::mem::transmute(objc_msgSend as *const ());
                key_window = f(windows, sel("objectAtIndex:"), 0);
            }
        }
        let window_layer = msg0(key_window, sel("layer"));

        // If the key window changed since we created the overlay (e.g. the app
        // picker's window was replaced by the launched game's window), detach and
        // drop the stale overlay so it is recreated on the *current* window.
        // Without this the overlay stays on the dead window and the new window
        // renders black.
        let mut overlay = OVERLAY_LAYER.load(Ordering::Relaxed);
        if !overlay.is_null()
            && !window_layer.is_null()
            && OVERLAY_WINDOW_LAYER.load(Ordering::Relaxed) != window_layer
        {
            msg0(overlay, sel("removeFromSuperlayer"));
            msg0(overlay, sel("release"));
            OVERLAY_LAYER.store(std::ptr::null_mut(), Ordering::Relaxed);
            overlay = std::ptr::null_mut();
        }

        // Create the overlay layer on first use (or after a window change).
        if overlay.is_null() && !window_layer.is_null() {
            let new_layer = msg0(cls("CALayer"), sel("layer"));
            let new_layer = msg0(new_layer, sel("retain"));
            let bounds: RectRaw = {
                let f: extern "C" fn(Id, *mut c_void) -> RectRaw =
                    std::mem::transmute(objc_msgSend as *const ());
                f(window_layer, sel("bounds"))
            };
            {
                let f: extern "C" fn(Id, *mut c_void, RectRaw) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(new_layer, sel("setFrame:"), bounds);
            }
            msg1(new_layer, sel("setContentsGravity:"), kCAGravityResizeAspect);
            {
                let f: extern "C" fn(Id, *mut c_void, f64) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(new_layer, sel("setZPosition:"), 10000.0);
            }
            msg1(window_layer, sel("addSublayer:"), new_layer);
            OVERLAY_LAYER.store(new_layer, Ordering::Relaxed);
            OVERLAY_WINDOW_LAYER.store(window_layer, Ordering::Relaxed);
            overlay = new_layer;
        }

        if !overlay.is_null() {
            // Assign contents without the implicit fade animation.
            let ct = cls("CATransaction");
            msg0(ct, sel("begin"));
            {
                let f: extern "C" fn(Id, *mut c_void, i8) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(ct, sel("setDisableActions:"), 1);
            }
            // Keep the overlay matching the (possibly rotated) window bounds, so
            // the presented frame fills the screen after an orientation change
            // instead of staying at the original (portrait) size.
            if !window_layer.is_null() {
                let bounds: RectRaw = {
                    let f: extern "C" fn(Id, *mut c_void) -> RectRaw =
                        std::mem::transmute(objc_msgSend as *const ());
                    f(window_layer, sel("bounds"))
                };
                let f: extern "C" fn(Id, *mut c_void, RectRaw) =
                    std::mem::transmute(objc_msgSend as *const ());
                f(overlay, sel("setFrame:"), bounds);
            }
            msg1(overlay, sel("setContents:"), img);
            msg0(ct, sel("commit"));
        }

        CGImageRelease(img);
        CGDataProviderRelease(provider);
        CGColorSpaceRelease(space);
    }
    drop(payload);
}

// ============================================================================
// iOS: import a 32-bit app from an .ipa via the Files document picker.
//
// The launcher's "Load .ipa" button calls ios_present_ipa_picker(), which puts a
// real UIDocumentPickerViewController on screen. When the user picks an .ipa we
// extract its Payload/*.app into the writable touchHLE_apps dir and record the
// path; the app-picker run loop polls ios_take_imported_app() and launches it.
// All Obj-C here is the *host* runtime (real iOS UIKit, not touchHLE's emulated
// UIKit). Everything fails gracefully (no panics) so a problem just means
// "nothing was imported".
// ============================================================================
#[cfg(target_os = "ios")]
mod ios_ipa_picker {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_void};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicPtr, Ordering};

    type Id = *mut c_void;
    type Sel = *mut c_void;

    static IMPORTED_APP_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    static DELEGATE_INSTANCE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
        fn objc_registerClassPair(cls: Id);
        fn class_addMethod(cls: Id, name: Sel, imp: *const c_void, types: *const c_char) -> bool;
    }

    unsafe fn sel(s: &str) -> Sel {
        sel_registerName(CString::new(s).unwrap().as_ptr())
    }
    unsafe fn cls(s: &str) -> Id {
        objc_getClass(CString::new(s).unwrap().as_ptr())
    }
    unsafe fn msg0(o: Id, s: Sel) -> Id {
        if o.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(o, s)
    }
    unsafe fn msg1(o: Id, s: Sel, a: Id) -> Id {
        if o.is_null() {
            return std::ptr::null_mut();
        }
        let f: extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(o, s, a)
    }
    unsafe fn ns_str(s: &str) -> Id {
        let c = CString::new(s).unwrap();
        let f: extern "C" fn(Id, Sel, *const c_char) -> Id =
            std::mem::transmute(objc_msgSend as *const ());
        f(cls("NSString"), sel("stringWithUTF8String:"), c.as_ptr())
    }
    unsafe fn ns_to_string(s: Id) -> Option<String> {
        if s.is_null() {
            return None;
        }
        let f: extern "C" fn(Id, Sel) -> *const c_char =
            std::mem::transmute(objc_msgSend as *const ());
        let p = f(s, sel("UTF8String"));
        if p.is_null() {
            return None;
        }
        Some(CStr::from_ptr(p).to_string_lossy().into_owned())
    }

    pub fn take_imported_app() -> Option<PathBuf> {
        IMPORTED_APP_PATH.lock().unwrap().take()
    }

    /// Show/hide the CoreAnimation overlay used to present touchHLE's own frames.
    /// It has a very high zPosition, so while a native modal (the Files picker) is
    /// up we must hide it or it covers the modal.
    unsafe fn set_overlay_hidden(hidden: bool) {
        let overlay = super::OVERLAY_LAYER.load(Ordering::Relaxed);
        if !overlay.is_null() {
            let f: extern "C" fn(Id, Sel, i8) = std::mem::transmute(objc_msgSend as *const ());
            f(overlay, sel("setHidden:"), if hidden { 1 } else { 0 });
        }
    }

    /// Extract Payload/*.app from an .ipa into the touchHLE_apps dir. Returns the
    /// path of the extracted .app on success.
    fn import_ipa(ipa_path: &Path) -> Option<PathBuf> {
        let apps_dir = crate::paths::user_data_base_path().join(crate::paths::APPS_DIR);
        std::fs::create_dir_all(&apps_dir).ok()?;
        let file = std::fs::File::open(ipa_path).ok()?;
        let mut zip = zip::ZipArchive::new(file).ok()?;
        let mut app_dir_name: Option<String> = None;
        for i in 0..zip.len() {
            let mut entry = match zip.by_index(i) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.name().replace('\\', "/");
            let rest = match name.strip_prefix("Payload/") {
                Some(r) if !r.is_empty() => r.to_string(),
                _ => continue,
            };
            if app_dir_name.is_none() {
                if let Some(first) = rest.split('/').next() {
                    if first.to_ascii_lowercase().ends_with(".app") {
                        app_dir_name = Some(first.to_string());
                    }
                }
            }
            let dest = apps_dir.join(&rest);
            if name.ends_with('/') {
                let _ = std::fs::create_dir_all(&dest);
            } else {
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut out) = std::fs::File::create(&dest) {
                    let _ = std::io::copy(&mut entry, &mut out);
                }
            }
        }
        app_dir_name.map(|n| apps_dir.join(n))
    }

    unsafe fn handle_url(url: Id) {
        if url.is_null() {
            return;
        }
        if let Some(path) = ns_to_string(msg0(url, sel("path"))) {
            eprintln!("[ios] Files picker selected: {}", path);
            match import_ipa(Path::new(&path)) {
                Some(app_path) => {
                    eprintln!("[ios] Imported app: {}", app_path.display());
                    *IMPORTED_APP_PATH.lock().unwrap() = Some(app_path);
                }
                None => eprintln!("[ios] Import failed (no Payload/*.app in the chosen file?)"),
            }
        }
    }

    unsafe fn dismiss(picker: Id) {
        // Restore the overlay we hid while the sheet was up.
        set_overlay_hidden(false);
        let f: extern "C" fn(Id, Sel, i8, Id) = std::mem::transmute(objc_msgSend as *const ());
        f(
            picker,
            sel("dismissViewControllerAnimated:completion:"),
            1,
            std::ptr::null_mut(),
        );
        let d = DELEGATE_INSTANCE.swap(std::ptr::null_mut(), Ordering::Relaxed);
        if !d.is_null() {
            msg0(d, sel("release"));
        }
    }

    // iOS 14+ plural callback.
    extern "C" fn did_pick_multi(_self: Id, _cmd: Sel, picker: Id, urls: Id) {
        unsafe {
            handle_url(msg0(urls, sel("firstObject")));
            dismiss(picker);
        }
    }
    // Older singular callback.
    extern "C" fn did_pick_single(_self: Id, _cmd: Sel, picker: Id, url: Id) {
        unsafe {
            handle_url(url);
            dismiss(picker);
        }
    }
    extern "C" fn did_cancel(_self: Id, _cmd: Sel, picker: Id) {
        unsafe {
            dismiss(picker);
        }
    }

    extern "C" fn do_present(_ctx: *mut c_void) {
        unsafe {
            eprintln!("[ios] doc-picker: do_present running on main thread");
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                let sc = cls("NSObject");
                let name = CString::new("touchHLEIpaPickerDelegate").unwrap();
                let nc = objc_allocateClassPair(sc, name.as_ptr(), 0);
                if !nc.is_null() {
                    let t2 = CString::new("v@:@@").unwrap();
                    class_addMethod(
                        nc,
                        sel("documentPicker:didPickDocumentsAtURLs:"),
                        did_pick_multi as *const c_void,
                        t2.as_ptr(),
                    );
                    class_addMethod(
                        nc,
                        sel("documentPicker:didPickDocumentAtURL:"),
                        did_pick_single as *const c_void,
                        t2.as_ptr(),
                    );
                    let t1 = CString::new("v@:@").unwrap();
                    class_addMethod(
                        nc,
                        sel("documentPickerWasCancelled:"),
                        did_cancel as *const c_void,
                        t1.as_ptr(),
                    );
                    objc_registerClassPair(nc);
                }
            });

            let dcls = cls("touchHLEIpaPickerDelegate");
            if dcls.is_null() {
                return;
            }
            let delegate = msg0(msg0(dcls, sel("alloc")), sel("init"));
            if delegate.is_null() {
                return;
            }
            msg0(delegate, sel("retain"));
            DELEGATE_INSTANCE.store(delegate, Ordering::Relaxed);

            // .ipa has no standard UTI, so accept generic data files.
            let types = msg1(cls("NSArray"), sel("arrayWithObject:"), ns_str("public.data"));

            let picker = msg0(cls("UIDocumentPickerViewController"), sel("alloc"));
            // initWithDocumentTypes:inMode:  (UIDocumentPickerModeImport == 0)
            let picker = {
                let f: extern "C" fn(Id, Sel, Id, u64) -> Id =
                    std::mem::transmute(objc_msgSend as *const ());
                f(picker, sel("initWithDocumentTypes:inMode:"), types, 0)
            };
            if picker.is_null() {
                return;
            }
            msg1(picker, sel("setDelegate:"), delegate);

            let app = msg0(cls("UIApplication"), sel("sharedApplication"));
            let mut win = msg0(app, sel("keyWindow"));
            if win.is_null() {
                let windows = msg0(app, sel("windows"));
                let count: usize = {
                    let f: extern "C" fn(Id, Sel) -> usize =
                        std::mem::transmute(objc_msgSend as *const ());
                    f(windows, sel("count"))
                };
                if count > 0 {
                    let f: extern "C" fn(Id, Sel, usize) -> Id =
                        std::mem::transmute(objc_msgSend as *const ());
                    win = f(windows, sel("objectAtIndex:"), 0);
                }
            }
            let root = msg0(win, sel("rootViewController"));
            if root.is_null() {
                eprintln!("[ios] doc-picker: no rootViewController; cannot present");
                return;
            }
            eprintln!("[ios] doc-picker: presenting Files picker over root VC");
            let f: extern "C" fn(Id, Sel, Id, i8, Id) =
                std::mem::transmute(objc_msgSend as *const ());
            f(
                root,
                sel("presentViewController:animated:completion:"),
                picker,
                1,
                std::ptr::null_mut(),
            );
            eprintln!("[ios] doc-picker: presentViewController call returned");
            // Hide our high-zPosition CoreAnimation overlay so it doesn't cover
            // the Files sheet. Restored in dismiss().
            set_overlay_hidden(true);
            eprintln!("[ios] doc-picker: overlay hidden so the sheet is visible");
        }
    }

    /// Present the Files document picker. UIKit must run on the main thread, but
    /// touchHLE's guest/launcher code runs on a coroutine stack (not the real
    /// main thread), so presenting directly hangs UIKit. Hop onto the main
    /// dispatch queue first (same approach as the CoreAnimation presenter).
    pub fn present() {
        extern "C" {
            static _dispatch_main_q: c_void;
            fn dispatch_async_f(
                queue: *const c_void,
                context: *mut c_void,
                work: extern "C" fn(*mut c_void),
            );
        }
        unsafe {
            dispatch_async_f(
                &_dispatch_main_q as *const c_void,
                std::ptr::null_mut(),
                do_present,
            );
        }
    }
}

/// iOS: present the Files document picker so the user can choose an `.ipa` to
/// import and launch. See [ios_ipa_picker].
#[cfg(target_os = "ios")]
pub fn ios_present_ipa_picker() {
    ios_ipa_picker::present();
}

/// iOS: take the path of an app just imported via the Files picker, if any.
#[cfg(target_os = "ios")]
pub fn ios_take_imported_app() -> Option<std::path::PathBuf> {
    ios_ipa_picker::take_imported_app()
}

/// iOS deep-link launch: a `touchhle://run?app=NAME` URL — e.g. opened by an iOS
/// Shortcut's "Open URL" action, LiveContainer-style — requests launching a
/// specific installed app inside the emulator. SDL hands such URLs to its app
/// delegate's `application:openURL:`, which it forwards as an `SDL_DROPFILE`
/// event; [Window::poll_for_events] parses it via [handle_possible_deeplink].
/// The app-picker run loop polls [ios_take_requested_launch] and launches the
/// named app.
static REQUESTED_DEEPLINK_APP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Parse a `touchhle://run?app=<percent-encoded name>` URL and record the
/// requested app name. No-op for anything else (e.g. a real dropped file path
/// on desktop), so it is safe to call for every `SDL_DROPFILE`.
fn handle_possible_deeplink(s: &str) {
    let Some(rest) = s.strip_prefix("touchhle://") else {
        return;
    };
    // `rest` is e.g. "run?app=JellyCar2.app"
    let query = rest.split_once('?').map_or(rest, |(_, q)| q);
    for kv in query.split('&') {
        if let Some(v) = kv.strip_prefix("app=") {
            let name = percent_decode(v);
            log!("Deep link requested launch of app: {:?}", name);
            *REQUESTED_DEEPLINK_APP.lock().unwrap() = Some(name);
            return;
        }
    }
    log!("Deep link {:?} had no app= parameter; ignoring", s);
}

/// iOS: take the app name requested via a `touchhle://run?app=` deep link, if any.
#[cfg(target_os = "ios")]
pub fn ios_take_requested_launch() -> Option<String> {
    REQUESTED_DEEPLINK_APP.lock().unwrap().take()
}

/// Minimal percent-decoding for deep-link query values (handles `%XX` and `+`).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hi = (b[i + 1] as char).to_digit(16);
                let lo = (b[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// iOS: probe whether the genuinely-native Home Screen icon route (generate a
/// per-game `.app` bundle and register it with SpringBoard via `uicache`) is
/// even possible from inside touchHLE's sandbox on this device. iOS forbids an
/// app from creating Home Screen icons except via Web Clips (configuration
/// profiles) or real installed app bundles; the bundle route needs elevated
/// access a sandboxed (TrollStore) app may not have. This logs its findings so
/// the native approach can be assessed on-device before it is built. No-op off
/// iOS.
#[cfg(target_os = "ios")]
pub fn ios_probe_home_screen_capability() {
    use std::path::Path;

    eprintln!("[home-probe] === Home Screen capability probe ===");
    unsafe {
        eprintln!(
            "[home-probe] uid={} euid={} (0 == root)",
            libc::getuid(),
            libc::geteuid()
        );
    }
    match std::env::current_exe() {
        Ok(p) => eprintln!("[home-probe] current_exe={}", p.display()),
        Err(e) => eprintln!("[home-probe] current_exe error: {e}"),
    }

    // Can we write into a jailbreak Applications directory (where a new .app
    // bundle would have to live to get a Home Screen icon)?
    for dir in [
        "/var/jb/Applications",
        "/Applications",
        "/var/jb/var/containers/Bundle/Application",
    ] {
        let exists = Path::new(dir).is_dir();
        let writable = exists && {
            let probe = Path::new(dir).join(".touchHLE_write_probe");
            match std::fs::write(&probe, b"x") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    true
                }
                Err(_) => false,
            }
        };
        eprintln!("[home-probe] dir {dir}: exists={exists} writable={writable}");
    }

    // Is a uicache binary present (needed to register a new app with SpringBoard)?
    let mut uicache_present = false;
    for bin in [
        "/var/jb/usr/bin/uicache",
        "/usr/bin/uicache",
        "/var/jb/bin/uicache",
    ] {
        let present = Path::new(bin).is_file();
        uicache_present |= present;
        eprintln!("[home-probe] uicache {bin}: present={present}");
    }

    // NOTE: intentionally read-only. We do NOT attempt fork/exec here — a strict
    // sandbox could turn that into a launch-time fault. Whether child-process
    // exec is allowed is tested separately/later once the signals below look
    // promising.
    eprintln!(
        "[home-probe] verdict: native .app-bundle route needs a writable Applications dir AND uicache (present={uicache_present}) AND (separately) child-process exec — review the flags above"
    );
}

/// Tell SDL2 what orientation we want. Only useful on Android.
fn set_sdl2_orientation(orientation: DeviceOrientation) {
    // Despite the name, this hint works on Android too.
    let value = match orientation {
        DeviceOrientation::Portrait => "Portrait",
        // The inversion is deliberate. These probably correspond to
        // iPhone OS content orientations?
        DeviceOrientation::PortraitUpsideDown => "PortraitUpsideDown",
        DeviceOrientation::LandscapeLeft => "LandscapeRight",
        DeviceOrientation::LandscapeRight => "LandscapeLeft",
    };
    log!(
        "[diag-orient] set_sdl2_orientation: SDL_IOS_ORIENTATIONS={:?} (orientation={:?})",
        value,
        orientation
    );
    sdl2::hint::set("SDL_IOS_ORIENTATIONS", value);
}

/// iOS-only: introspect orientation: see the canonical `ios_dump_orientation`
/// defined earlier in this file.

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum FingerId {
    Mouse,
    Touch(i64),
    VirtualCursor,
    ButtonToTouch(crate::options::Button),
    StickToTouch,
    DpadToTouch,
}
pub type Coords = (f32, f32);

struct DpadState {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    active: bool,
}

#[derive(Debug)]
pub enum TextInputEvent {
    Text(String),
    Backspace,
    Return,
}

#[derive(Debug)]
pub enum Event {
    /// User requested quit.
    Quit,
    /// OS has informed touchHLE it will soon become inactive.
    /// (iOS `applicationWillResignActive:`, Android `onPause()`)
    AppWillResignActive,
    /// OS has informed touchHLE it will soon terminate.
    /// (iOS `applicationWillTerminate:`, Android `onDestroy()`)
    AppWillTerminate,
    TouchesDown(HashMap<FingerId, Coords>),
    TouchesMove(HashMap<FingerId, Coords>),
    TouchesUp(HashMap<FingerId, Coords>),
    /// User pressed F12, requesting that execution be paused and the debugger
    /// take over.
    EnterDebugger,
    TextInput(TextInputEvent),
}

pub enum BatteryState {
    Unknown,
    OnBattery,
    NoBattery,
    Charging,
    Full,
}

pub enum GLVersion {
    /// OpenGL ES 1.1
    GLES11,
    /// OpenGL 2.1 compatibility profile
    GL21Compat,
}

pub struct GLContext(sdl2::video::GLContext);

impl GLContext {
    pub fn is_current(&self) -> bool {
        self.0.is_current()
    }
}

fn surface_from_image(image: &Image) -> Surface<'_> {
    let src_pixels = image.pixels();
    let (width, height) = image.dimensions();

    let mut surface = Surface::new(width, height, PixelFormatEnum::RGBA32).unwrap();
    let (width, height) = (width as usize, height as usize);
    let pitch = surface.pitch() as usize;
    surface.with_lock_mut(|dst_pixels| {
        for y in 0..height {
            for x in 0..width {
                for channel in 0..4 {
                    let src_idx = y * width * 4 + x * 4 + channel;
                    let dst_idx = y * pitch + x * 4 + channel;
                    dst_pixels[dst_idx] = src_pixels[src_idx];
                }
            }
        }
    });
    surface
}

pub struct Window {
    _sdl_ctx: sdl2::Sdl,
    video_ctx: sdl2::VideoSubsystem,
    window: sdl2::video::Window,
    event_pump: sdl2::EventPump,
    event_queue: VecDeque<Event>,
    last_polled: Instant,
    /// Separate queue for extremely high-priority events (e.g. app about to
    /// terminate).
    high_priority_event: Option<Event>,
    enable_event_polling: bool,
    #[cfg(target_os = "macos")]
    max_height: u32,
    #[cfg(target_os = "macos")]
    viewport_y_offset: u32,
    /// Copy of `fullscreen` on [Options]. Note that this is meaningless when
    /// [Self::rotatable_fullscreen] returns [true].
    fullscreen: bool,
    scale_hack: NonZeroU32,
    internal_gl_ins: Option<Box<dyn GLESContext>>,
    /// Framebuffer object representing the visible screen. This is 0 (the window
    /// default framebuffer) on desktop, but on iOS SDL backs the screen with a
    /// non-zero FBO attached to the CAEAGLLayer, so we capture whatever SDL
    /// leaves bound at context creation and present into that instead of 0.
    gl_default_framebuffer: u32,
    splash_image: Option<Image>,
    device_family: DeviceFamily,
    device_orientation: DeviceOrientation,
    force_portrait: bool,
    controller_ctx: sdl2::GameControllerSubsystem,
    controllers: Vec<sdl2::controller::GameController>,
    dpad_state: DpadState,
    stick_active: bool,
    _sensor_ctx: sdl2::SensorSubsystem,
    accelerometer: Option<sdl2::sensor::Sensor>,
    virtual_cursor_last: Option<(f32, f32, bool, bool)>,
    virtual_cursor_last_unsticky: Option<(f32, f32, Instant)>,
    virtual_accelerometer_last: Option<(f32, f32, bool)>,
    /// Whether or not we are on the "main" environment stack (rather than
    /// a coroutine stack). Checked in various functions to make sure that
    /// certain SDL functions (that call JNI functions) are on the main
    /// stack on Android.
    pub(super) on_main_stack: bool,
}

impl Window {
    /// Returns [true] if touchHLE is running on a device where we should always
    /// display fullscreen, but SDL2 will let us control the orientation, i.e.
    /// Android and iOS devices.
    pub fn rotatable_fullscreen() -> bool {
        env::consts::OS == "android" || env::consts::OS == "ios"
    }
    pub fn new(
        title: &str,
        icon: Option<Image>,
        launch_image: Option<Image>,
        options: &Options,
    ) -> Window {
        let sdl_ctx = sdl2::init().unwrap();
        let video_ctx = sdl_ctx.video().unwrap();

        // The "hidapi" feature of rust-sdl2 is enabled so that sdl2::sensor
        // is available, but we don't want to enable SDL's HIDAPI controller
        // drivers because they cause duplicated controllers on macOS
        // (https://github.com/libsdl-org/SDL/issues/7479). Once that's fixed,
        // remove this (https://github.com/touchHLE/touchHLE/issues/85).
        sdl2::hint::set("SDL_JOYSTICK_HIDAPI", "0");

        if env::consts::OS == "android" || env::consts::OS == "ios" {
            // It's important to set context version BEFORE window creation
            // ref. https://wiki.libsdl.org/SDL2/SDL_GLattr
            // Mobile platforms (Android, iOS) provide OpenGL ES only; touchHLE's
            // host renderer targets GLES 1.1.
            let attr = video_ctx.gl_attr();
            attr.set_context_version(1, 1);
            attr.set_context_profile(sdl2::video::GLProfile::GLES);
        }

        if env::consts::OS == "android" {
            // Disable blocking of event loop when app is paused.
            sdl2::hint::set("SDL_ANDROID_BLOCK_ON_PAUSE", "0");
        }

        // Separate mouse and touch events
        sdl2::hint::set("SDL_TOUCH_MOUSE_EVENTS", "0");

        // SDL2 disables the screen saver by default, but iPhone OS enables
        // the idle timer that triggers sleep by default, so we turn it back on
        // here, and then the app can disable it if it wants to.
        video_ctx.enable_screen_saver();

        let scale_hack = options.scale_hack;
        // TODO: some apps specify their orientation in Info.plist, we could use
        // that here.
        let device_family = options.device_family.unwrap_or(DeviceFamily::iPhone);
        let device_orientation = options.initial_orientation;
        let fullscreen = options.fullscreen;

        let mut window = if Self::rotatable_fullscreen() {
            // Without this, SDL will force fullscreen mode to be portrait.
            set_sdl2_orientation(device_orientation);
            let screen_size = video_ctx.display_bounds(0).unwrap().size();
            let (width, height) = rotate_fullscreen_size(device_orientation, screen_size);
            let window = video_ctx
                .window(title, width, height)
                .fullscreen()
                .opengl()
                .build()
                .unwrap();
            window
        } else if fullscreen {
            let (width, height) = video_ctx.display_bounds(0).unwrap().size();
            let window = video_ctx
                .window(title, width, height)
                .fullscreen_desktop()
                .opengl()
                .build()
                .unwrap();
            window
        } else {
            let (width, height) =
                size_for_orientation(device_family, device_orientation, scale_hack);
            let window = video_ctx
                .window(title, width, height)
                .position_centered()
                .opengl()
                .build()
                .unwrap();
            window
        };

        #[cfg(target_os = "ios")]
        {
            let (ww, wh) = window.size();
            let (dw, dh) = window.drawable_size();
            log!(
                "[diag-orient] window created: device_orientation={:?} rotatable_fullscreen={} requested_via=rotate_fullscreen_size window.size={}x{} drawable={}x{}",
                device_orientation,
                Self::rotatable_fullscreen(),
                ww,
                wh,
                dw,
                dh
            );
        }

        if env::consts::OS == "android" {
            // Sanity check
            let gl_attr = video_ctx.gl_attr();
            debug_assert_eq!(gl_attr.context_profile(), sdl2::video::GLProfile::GLES);
            debug_assert_eq!(gl_attr.context_version(), (1, 1));
        }

        if let Some(icon) = icon {
            window.set_icon(surface_from_image(&icon));
        }

        let event_pump = sdl_ctx.event_pump().unwrap();

        let controller_ctx = sdl_ctx.game_controller().unwrap();

        let sensor_ctx = sdl_ctx.sensor().unwrap();
        let mut accelerometer: Option<sdl2::sensor::Sensor> = None;
        if let Ok(num_sensors) = sensor_ctx.num_sensors() {
            for sensor_idx in 0..num_sensors {
                if let Ok(sensor) = sensor_ctx.open(sensor_idx) {
                    if sensor.sensor_type() == sdl2::sensor::SensorType::Accelerometer {
                        log!("Accelerometer detected: {}.", sensor.name());
                        accelerometer = Some(sensor);
                        break;
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        let max_height = window.size().1;

        let mut window = Window {
            _sdl_ctx: sdl_ctx,
            video_ctx,
            window,
            event_pump,
            event_queue: VecDeque::new(),
            last_polled: Instant::now() - Duration::from_secs(1),
            high_priority_event: None,
            enable_event_polling: true,
            #[cfg(target_os = "macos")]
            max_height,
            #[cfg(target_os = "macos")]
            viewport_y_offset: 0,
            fullscreen,
            scale_hack,
            internal_gl_ins: None,
            gl_default_framebuffer: 0,
            splash_image: launch_image,
            device_family,
            device_orientation,
            force_portrait: options.force_portrait,
            controller_ctx,
            controllers: Vec::new(),
            dpad_state: DpadState {
                left: false,
                right: false,
                up: false,
                down: false,
                active: false,
            },
            stick_active: false,
            _sensor_ctx: sensor_ctx,
            accelerometer,
            virtual_cursor_last: None,
            virtual_cursor_last_unsticky: None,
            virtual_accelerometer_last: None,
            on_main_stack: true,
        };

        // Set up OpenGL ES context used for splash screen and app UI rendering
        // (see src/frameworks/core_animation/composition.rs). OpenGL ES is used
        // because SDL2 won't let us use more than one graphics API in the same
        // window, and we also need OpenGL ES for the app's own rendering.
        let mut gl_ins = create_gles1_ctx_no_parent_stack(&mut window, options);
        let screen_fbo: u32 = {
            let mut gl_ctx = gl_ins.make_current(&mut window);
            log!("Driver info: {}", unsafe { gl_ctx.driver_description() });
            // On iOS the visible screen is backed by a non-zero framebuffer
            // (SDL's CAEAGLLayer FBO), not framebuffer 0. Capture whatever SDL
            // left bound now, while none of our own framebuffers are bound, so
            // the compositor can present into the real screen framebuffer rather
            // than 0 (which renders to nothing on iOS). On desktop this is 0.
            use crate::gles::gles11_raw as gldiag;
            let mut fbo: i32 = 0;
            let mut rbo: i32 = 0;
            let mut vp = [0i32; 4];
            let mut rb_w: i32 = -1;
            let mut rb_h: i32 = -1;
            let fb_status;
            let gl_err;
            unsafe {
                gl_ctx.GetIntegerv(gldiag::FRAMEBUFFER_BINDING_OES, &mut fbo);
                gl_ctx.GetIntegerv(gldiag::RENDERBUFFER_BINDING_OES, &mut rbo);
                gl_ctx.GetIntegerv(gldiag::VIEWPORT, vp.as_mut_ptr());
                fb_status = gl_ctx.CheckFramebufferStatusOES(gldiag::FRAMEBUFFER_OES);
                gl_ctx.GetRenderbufferParameterivOES(
                    gldiag::RENDERBUFFER_OES,
                    gldiag::RENDERBUFFER_WIDTH_OES,
                    &mut rb_w,
                );
                gl_ctx.GetRenderbufferParameterivOES(
                    gldiag::RENDERBUFFER_OES,
                    gldiag::RENDERBUFFER_HEIGHT_OES,
                    &mut rb_h,
                );
                gl_err = gl_ctx.GetError();
            }
            log!(
                "[diag] GL state: screen_fbo={} bound_rbo={} fb_status=0x{:x} (complete=0x{:x}) screen_rbo_size={}x{} viewport={:?} gl_err=0x{:x}",
                fbo, rbo, fb_status, gldiag::FRAMEBUFFER_COMPLETE_OES, rb_w, rb_h, vp, gl_err
            );
            fbo.max(0) as u32
        };
        window.gl_default_framebuffer = screen_fbo;
        window.internal_gl_ins = Some(gl_ins);

        // === iOS display diagnostics: SDL window / view state at creation ===
        {
            let flags = window.window.window_flags();
            let (sw, sh) = window.window.size();
            let (dw, dh) = window.window.drawable_size();
            let (px, py) = window.window.position();
            log!(
                "[diag] SDL window: flags=0x{:08x} SHOWN={} HIDDEN={} MINIMIZED={} FULLSCREEN={} OPENGL={} METAL={} HIGHDPI={}",
                flags,
                flags & 0x0000_0004 != 0,
                flags & 0x0000_0008 != 0,
                flags & 0x0000_0040 != 0,
                flags & 0x0000_0001 != 0,
                flags & 0x0000_0002 != 0,
                flags & 0x2000_0000 != 0,
                flags & 0x0000_2000 != 0,
            );
            log!(
                "[diag] SDL window: logical_size={}x{} drawable={}x{} position=({},{}) default_fb={}",
                sw, sh, dw, dh, px, py, screen_fbo
            );
            #[cfg(target_os = "ios")]
            unsafe {
                ios_dump_view_hierarchy("at-creation");
            }
        }

        if window.splash_image.is_some() {
            window.display_splash();
        }

        // On iOS, touchHLE's main runs synchronously from the SDL app delegate's
        // postFinishLaunch and immediately enters a busy render loop, so UIKit
        // never gets a turn to finish presenting/laying out SDL's GL view
        // (SDL_uikitviewcontroller). The result is that the CAEAGLLayer is never
        // composited and the screen shows only the window's bare background.
        // Pump the iOS run loop for a few frames here so the view controller's
        // appearance transition completes and the layer is laid out before we
        // start rendering.
        #[cfg(target_os = "ios")]
        {
            for _ in 0..15 {
                window.event_pump.pump_events();
                std::thread::sleep(Duration::from_millis(16));
            }
        }

        window
    }

    /// Poll for events from the OS. This needs to be done reasonably often
    /// (60Hz is probably fine) so that the host OS doesn't consider touchHLE
    /// to be unresponsive. Note that events are not returned by this function,
    /// since we often need to defer actually handling them.
    ///
    /// Since polling can be quite expensive, this function will skip it if it
    /// was called too recently.
    pub fn poll_for_events(&mut self, options: &Options) {
        assert!(self.on_main_stack);
        let now = Instant::now();
        // poll roughly twice per frame to try to avoid missing frames sometimes
        if now.duration_since(self.last_polled) < Duration::from_secs_f64(1.0 / 120.0) {
            return;
        }
        self.last_polled = now;

        fn transform_input_coords(
            window: &Window,
            (in_x, in_y): (f32, f32),
            independent_of_viewport: bool,
        ) -> (f32, f32) {
            let (vx, vy, vw, vh) = if independent_of_viewport {
                let (width, height) = size_for_orientation(
                    window.device_family,
                    window.device_orientation,
                    NonZeroU32::new(1).unwrap(),
                );
                (0, 0, width, height)
            } else {
                window.viewport()
            };
            // normalize to unit square centred on origin
            let x = (in_x - vx as f32) / vw as f32 - 0.5;
            let y = (in_y - vy as f32) / vh as f32 - 0.5;
            // rotate
            let matrix = window.rotation_matrix().inverse().unwrap();
            let [x, y] = matrix.transform([x, y]);
            // back to pixels
            let (out_w, out_h) = window.size_unrotated_unscaled();
            let out_x = (x + 0.5) * out_w as f32;
            let out_y = (y + 0.5) * out_h as f32;
            // Round to match touch precision of official devices.
            (out_x.round(), out_y.round())
        }
        fn transform_virt_accel_coords(window: &Window, (in_x, in_y): (i32, i32)) -> (f32, f32) {
            let (_, _, vw, vh) = window.viewport();
            let out_x = ((in_x as f32 / vw as f32) * 2.0 - 1.0).clamp(-1.0, 1.0);
            let out_y = ((in_y as f32 / vh as f32) * 2.0 - 1.0).clamp(-1.0, 1.0);
            (out_x, out_y)
        }
        fn translate_button(button: sdl2::controller::Button) -> Option<crate::options::Button> {
            match button {
                sdl2::controller::Button::DPadLeft => Some(crate::options::Button::DPadLeft),
                sdl2::controller::Button::DPadUp => Some(crate::options::Button::DPadUp),
                sdl2::controller::Button::DPadRight => Some(crate::options::Button::DPadRight),
                sdl2::controller::Button::DPadDown => Some(crate::options::Button::DPadDown),
                sdl2::controller::Button::Start => Some(crate::options::Button::Start),
                sdl2::controller::Button::A => Some(crate::options::Button::A),
                sdl2::controller::Button::B => Some(crate::options::Button::B),
                sdl2::controller::Button::X => Some(crate::options::Button::X),
                sdl2::controller::Button::Y => Some(crate::options::Button::Y),
                sdl2::controller::Button::LeftShoulder => {
                    Some(crate::options::Button::LeftShoulder)
                }
                _ => None,
            }
        }
        fn finger_absolute_coords(window: &Window, (x, y): (f32, f32)) -> (f32, f32) {
            let (screen_width, screen_height) = window.window.drawable_size();
            (screen_width as f32 * x, screen_height as f32 * y)
        }

        let mut controller_updated = false;
        // event_pump doesn't have a method to peek on events
        // so, we keep track of an unconsumed one from a previous loop iteration
        // FIXME: use peek_event() from even_subsystem
        let mut previous_event: Option<sdl2::event::Event> = None;
        while self.enable_event_polling {
            use sdl2::event::Event as E;
            let event = if let Some(e) = previous_event.take() {
                match e {
                    E::Unknown { .. } => (),
                    _ => log_dbg!("Consuming previous event: {:?}", e),
                }
                e
            } else if let Some(e) = self.event_pump.poll_event() {
                match e {
                    E::Unknown { .. } => (),
                    _ => log_dbg!("Consuming new event: {:?}", e),
                }
                e
            } else {
                break;
            };

            // Virtual accelerometer
            match event {
                E::MouseButtonDown {
                    x,
                    y,
                    mouse_btn: MouseButton::Right,
                    ..
                } => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, true));
                }
                E::MouseMotion {
                    x, y, mousestate, ..
                } if mousestate.right() => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, true));
                }
                E::MouseButtonUp {
                    x,
                    y,
                    mouse_btn: MouseButton::Right,
                    ..
                } => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, false));
                }
                // iOS delivers `touchhle://run?app=...` deep links (and desktop
                // delivers real file drops) as a DROPFILE event. Parse our
                // scheme here; non-matching strings are ignored.
                E::DropFile { ref filename, .. } => {
                    handle_possible_deeplink(filename);
                }
                _ => {}
            }

            self.event_queue.push_back(match event {
                E::Quit { .. } => Event::Quit,
                E::MouseButtonDown {
                    x,
                    y,
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("MouseButtonDown x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesDown(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseMotion {
                    x, y, mousestate, ..
                } if mousestate.left() => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("MouseMotion x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesMove(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseButtonUp {
                    x,
                    y,
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("MouseButtonUp x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesUp(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::ControllerDeviceAdded { which, .. } => {
                    self.controller_added(which);
                    continue;
                }
                E::ControllerDeviceRemoved { which, .. } => {
                    self.controller_removed(which);
                    continue;
                }
                // Note that accelerometer simulation with analog sticks is
                // handled with polling, rather than being event-based.
                E::ControllerButtonUp { button, .. } | E::ControllerButtonDown { button, .. } => {
                    controller_updated = true;
                    let Some(button) = translate_button(button) else {
                        continue;
                    };
                    // Called whenever a DPad direction is pressed or released
                    if (button == crate::options::Button::DPadLeft
                        || button == crate::options::Button::DPadUp
                        || button == crate::options::Button::DPadRight
                        || button == crate::options::Button::DPadDown)
                        && options.dpad_to_touch.is_some()
                    {
                        let Some((x, y, w, h)) = options.dpad_to_touch else {
                            unreachable!();
                        };

                        // Update held state
                        let pressed = matches!(event, E::ControllerButtonDown { .. });
                        match button {
                            crate::options::Button::DPadLeft => self.dpad_state.left = pressed,
                            crate::options::Button::DPadRight => self.dpad_state.right = pressed,
                            crate::options::Button::DPadUp => self.dpad_state.up = pressed,
                            crate::options::Button::DPadDown => self.dpad_state.down = pressed,
                            _ => unreachable!(),
                        }

                        // Compute center
                        let cx = x + w * 0.5;
                        let cy = y + h * 0.5;

                        // Compute combined delta
                        let mut dx = 0.0;
                        let mut dy = 0.0;

                        if self.dpad_state.left {
                            dx -= 0.5 * w;
                        }
                        if self.dpad_state.right {
                            dx += 0.5 * w;
                        }
                        if self.dpad_state.up {
                            dy -= 0.5 * h;
                        }
                        if self.dpad_state.down {
                            dy += 0.5 * h;
                        }

                        // Final coords: center + movement
                        let coords = transform_input_coords(self, (cx + dx, cy + dy), true);

                        // Send TouchDown if any dpad is held, TouchUp if none
                        let any_held = self.dpad_state.left
                            || self.dpad_state.right
                            || self.dpad_state.up
                            || self.dpad_state.down;

                        if !self.dpad_state.active && any_held {
                            // New touch
                            self.dpad_state.active = true;
                            Event::TouchesDown(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else if self.dpad_state.active && any_held {
                            // Move existing touch
                            Event::TouchesMove(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else if self.dpad_state.active && !any_held {
                            // Release touch
                            self.dpad_state.active = false;
                            Event::TouchesUp(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else {
                            continue;
                        }
                    } else {
                        let Some(&(x, y)) = options.button_to_touch.get(&button) else {
                            continue;
                        };
                        match event {
                            E::ControllerButtonUp { .. } => {
                                let coords = transform_input_coords(self, (x, y), true);
                                Event::TouchesUp(HashMap::from([(
                                    FingerId::ButtonToTouch(button),
                                    coords,
                                )]))
                            }
                            E::ControllerButtonDown { .. } => {
                                let coords = transform_input_coords(self, (x, y), true);
                                Event::TouchesDown(HashMap::from([(
                                    FingerId::ButtonToTouch(button),
                                    coords,
                                )]))
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                E::ControllerAxisMotion { axis, .. } => {
                    controller_updated = true;
                    let Some((x, y, w, h)) = options.stick_to_touch else {
                        continue;
                    };
                    if axis == sdl2::controller::Axis::LeftX
                        || axis == sdl2::controller::Axis::LeftY
                    {
                        let (stick_x, stick_y, _) = self.get_controller_stick(options, true);
                        let coords = transform_input_coords(
                            self,
                            (
                                x + ((stick_x + 1.0) / 2.0) * w,
                                y + ((stick_y + 1.0) / 2.0) * h,
                            ),
                            true,
                        );
                        if stick_x.abs() < options.deadzone && stick_y.abs() < options.deadzone {
                            if !self.stick_active {
                                // Ignore deadzone events when stick is inactive
                                continue;
                            } else {
                                // Release touch when stick returns to deadzone
                                self.stick_active = false;
                                Event::TouchesUp(HashMap::from([(FingerId::StickToTouch, coords)]))
                            }
                        } else if !self.stick_active {
                            // New touch
                            self.stick_active = true;
                            Event::TouchesDown(HashMap::from([(FingerId::StickToTouch, coords)]))
                        } else {
                            // Move existing touch
                            Event::TouchesMove(HashMap::from([(FingerId::StickToTouch, coords)]))
                        }
                    } else {
                        continue;
                    }
                }
                E::AppWillEnterBackground { .. } => {
                    if env::consts::OS == "ios" {
                        // iOS fires a spurious applicationWillResignActive during
                        // the launch presentation (the SDL view-controller
                        // appearance-transition churn). Treating it as a quit (as
                        // below) kills the app at startup before anything renders.
                        // Ignore background transitions on iOS for now so the app
                        // stays alive and keeps rendering.
                        // TODO: properly pause/resume audio and state instead.
                        log!("Ignoring app-will-enter-background event on iOS.");
                        continue;
                    }
                    log!("Received app-will-resign-active event.");
                    assert!(self.high_priority_event.is_none());
                    self.high_priority_event = Some(Event::AppWillResignActive);
                    // For some reason, if we don't pause event polling, we will
                    // never finish handling the event.
                    // TODO: Add a mechanism for re-enabling polling, if at some
                    // point we support returning touchHLE to the foreground.
                    self.enable_event_polling = false;
                    continue;
                }
                E::AppTerminating { .. } => {
                    log!("Received app-will-terminate event.");
                    assert!(self.high_priority_event.is_none());
                    self.high_priority_event = Some(Event::AppWillTerminate);
                    self.enable_event_polling = false;
                    continue;
                }
                E::FingerUp {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                }
                | E::FingerMotion {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                }
                | E::FingerDown {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                } => {
                    log_dbg!("Starting multi-touch for {:?}", event);
                    // To implement multi-touch we accumulate here same touch
                    // events at the same timestamp. This is consistent with
                    // UIKit, but could be broken if events come out of order.
                    // (in worst case we separate multi-touches in several ones)
                    // TODO: handle out of order touches
                    let curr_timestamp = timestamp;
                    let abs_coords = finger_absolute_coords(self, (x, y));
                    let coords = transform_input_coords(self, abs_coords, false);
                    log_dbg!("Finger event x {}, y {}, coords {:?}", x, y, coords);
                    let mut map = HashMap::from([(FingerId::Touch(finger_id), coords)]);
                    while let Some(next) = self.event_pump.poll_event() {
                        match next {
                            E::Unknown { .. } => (),
                            _ => log_dbg!("Next possible multi-touch event: {:?}", next),
                        }
                        match next {
                            E::FingerUp {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            }
                            | E::FingerMotion {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            }
                            | E::FingerDown {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            } if timestamp == curr_timestamp && next.is_same_kind_as(&event) => {
                                let abs_coords = finger_absolute_coords(self, (x, y));
                                let coords = transform_input_coords(self, abs_coords, false);
                                map.insert(FingerId::Touch(finger_id), coords);
                            }
                            E::MultiGesture { timestamp, .. } if timestamp == curr_timestamp => {
                                // TODO: handle gestures
                                continue;
                            }
                            _ => {
                                // event_pump doesn't have a method to peek on
                                // events, so we keep track of an unconsumed
                                // one from a previous loop iteration
                                assert!(previous_event.is_none());
                                previous_event = Some(next);
                                break;
                            }
                        }
                    }
                    log_dbg!("Finishing multi-touch for {:?} with {:?}", event, map);
                    match event {
                        E::FingerUp { .. } => Event::TouchesUp(map),
                        E::FingerMotion { .. } => Event::TouchesMove(map),
                        E::FingerDown { .. } => Event::TouchesDown(map),
                        _ => unreachable!(),
                    }
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::F12),
                    ..
                } => {
                    // Log this so you can tell when touchHLE has received
                    // the event but it's stuck in the queue.
                    echo!("F12 pressed, EnterDebugger event queued.");
                    Event::EnterDebugger
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Backspace),
                    ..
                } => {
                    log_dbg!("SDL TextInput Backspace");
                    Event::TextInput(TextInputEvent::Backspace)
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Return),
                    ..
                } => {
                    log_dbg!("SDL TextInput Return");
                    Event::TextInput(TextInputEvent::Return)
                }
                E::TextInput { text, .. } => {
                    log_dbg!("SDL TextInput {}", text);
                    Event::TextInput(TextInputEvent::Text(text))
                }
                _ => continue,
            })
        }

        if controller_updated {
            let (new_x, new_y, pressed, pressed_changed, moved) =
                self.update_virtual_cursor(options);
            self.event_queue
                .push_back(match (pressed, pressed_changed, moved) {
                    (true, true, _) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesDown(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    (false, true, _) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesUp(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    (true, _, true) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesMove(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    _ => return,
                });
        }
    }

    /// Pop an event from the queue (in FIFO order, except for high priority
    /// events)
    pub fn pop_event(&mut self) -> Option<Event> {
        self.high_priority_event
            .take()
            .or_else(|| self.event_queue.pop_front())
    }

    fn controller_added(&mut self, joystick_idx: u32) {
        let Ok(controller) = self.controller_ctx.open(joystick_idx) else {
            log!("Warning: A new controller was connected, but it couldn't be accessed!");
            return;
        };

        let controller_name = controller.name();
        if env::consts::OS == "android" && controller_name.starts_with("uinput-") {
            log!("ignoring fingerprint device: {}", controller_name);
            return;
        }
        log!(
            "New controller connected: {}. Left stick = device tilt. Right stick = touch input (press the stick or shoulder button to tap/hold).",
            controller_name
        );
        self.controllers.push(controller);
    }
    fn controller_removed(&mut self, instance_id: u32) {
        let Some(idx) = self
            .controllers
            .iter()
            .position(|controller| controller.instance_id() == instance_id)
        else {
            return;
        };
        let controller = self.controllers.remove(idx);
        log!("Warning: Controller disconnected: {}", controller.name());
    }
    pub fn print_accelerometer_notice(&self, options: &Options) {
        log!("This app uses the accelerometer.");

        if !self.controllers.is_empty() && options.analog_stick_tilt_controls {
            log!("Your connected controller's left analog stick will be used for accelerometer simulation.");
            if self.accelerometer.is_some() {
                log!("Disconnect the controller if you want to use your device's accelerometer.");
            }
        } else if self.accelerometer.is_some() {
            log!("Your device's accelerometer will be used for accelerometer simulation.");
            if options.analog_stick_tilt_controls {
                log!("Connect a controller if you would prefer to use an analog stick.");
            }
        } else if self.controllers.is_empty() && options.analog_stick_tilt_controls {
            log!("Connect a controller to get accelerometer simulation.");
        }

        if self.accelerometer.is_none() {
            log!(
                "You can {}hold right click and move the cursor to simulate the accelerometer.",
                if options.analog_stick_tilt_controls {
                    "also "
                } else {
                    ""
                }
            );
        }
    }

    /// Get the real or simulated accelerometer output.
    /// See also [crate::frameworks::uikit::ui_accelerometer].
    pub fn get_acceleration(&self, options: &Options) -> (f32, f32, f32) {
        if self.controllers.is_empty() || !options.analog_stick_tilt_controls {
            if let Some(ref accelerometer) = self.accelerometer {
                let data = accelerometer.get_data().unwrap();
                let sdl2::sensor::SensorData::Accel(data) = data else {
                    panic!();
                };
                let [x, y, z] = data;
                // UIAcceleration reports acceleration towards gravity, but SDL2
                // reports acceleration away from gravity.
                let (x, y, z) = (-x, -y, -z);
                // UIAcceleration reports acceleration in units of g-force, but
                // SDL2 reports acceleration in units of m/s^2.
                let gravity: f32 = 9.80665; // SDL_STANDARD_GRAVITY
                let (x, y, z) = (x / gravity, y / gravity, z / gravity);
                return (x, y, z);
            }
        }

        let (x, y) = if self
            .virtual_accelerometer_last
            .is_some_and(|(_x, _y, right_click_hold)| right_click_hold)
        {
            self.virtual_accelerometer_last
                .map(|(x, y, _right_click_hold)| (x, y))
                .unwrap()
        } else {
            // Get left analog stick input. The range is [-1, 1] on each axis.
            let (x, y, _) = self.get_controller_stick(options, true);
            (x, y)
        };

        // Correct for window rotation
        let [x, y] = self.rotation_matrix().inverse().unwrap().transform([x, y]);
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0)); // just in case

        // Let's simulate tilting the device based on the analog stick inputs.
        //
        // If an iPhone is lying flat on its back, level with the ground, and it
        // is on Earth, the accelerometer will report approximately (0, 0, -1).
        // The acceleration x and y axes are aligned with the screen's x and y
        // axes. +x points to the right of the screen, +y points to the top of
        // the screen, and +z points away from the screen. In the example
        // scenario, the z axis is parallel to gravity.

        let gravity: [f32; 3] = [0.0, 0.0, -1.0];

        let neutral_x = options.x_tilt_offset.to_radians();
        let neutral_y = options.y_tilt_offset.to_radians();
        let x_rotation_range = options.x_tilt_range.to_radians() / 2.0;
        let y_rotation_range = options.y_tilt_range.to_radians() / 2.0;
        // (x, y) are swapped because the controller Y axis usually corresponds
        // to forward/backward movement, but rotating about the Y axis means
        // tilting the device left/right.
        let x_rotation = neutral_x - x_rotation_range * y;
        let y_rotation = neutral_y - y_rotation_range * x;
        let matrix =
            Matrix::<3>::y_rotation(y_rotation).multiply(&Matrix::<3>::x_rotation(x_rotation));
        let [x, y, z] = matrix.transform(gravity);

        (x, y, z)
    }

    /// For use when redrawing the screen: Get the cached on-screen position and
    /// press state of the analog stick-controlled virtual cursor, if it is
    /// visible.
    pub fn virtual_cursor_visible_at(&self) -> Option<(f32, f32, bool)> {
        let (x, y, pressed, visible) = self.virtual_cursor_last?;
        if visible {
            // When stickyness is in use, the visual cursor movement appears
            // uncomfortably choppy. Showing the un-sticky position is a bit
            // misleading but it *feels* better, and it is documented.
            if let Some((x_unsticky, y_unsticky, _time)) = self.virtual_cursor_last_unsticky {
                Some((x_unsticky, y_unsticky, pressed))
            } else {
                Some((x, y, pressed))
            }
        } else {
            None
        }
    }

    /// Update the virtual cursor's position, click state and visibility, then
    /// return the new position, pressed state, whether the press state changed
    /// and whether the cursor moved.
    fn update_virtual_cursor(&mut self, options: &Options) -> (f32, f32, bool, bool, bool) {
        // Get right analog stick input. The range is [-1, 1] on each axis.
        let (x, y, pressed) = self.get_controller_stick(options, false);

        // The cursor is intended to only show up once you move the analog stick
        // out of its deadzone, or while the button is held.
        let visible = pressed || x != 0.0 || y != 0.0;

        // Though the analog stick output fits within a square, its actual range
        // is usually a circle enclosed by the square. So we need to cut out the
        // rectangular shape of the screen from that circle within the square.
        let (vx, vy, vw, vh) = self.viewport();
        let (vx, vy, vw, vh) = (vx as f32, vy as f32, vw as f32, vh as f32);

        let (x, y) = {
            // Use Pythagoras's theorem to find the largest size the rectangle
            // can have within the circle.
            let ratio = vw / vh;
            let rect_height = (ratio * ratio + 1.0).powf(-0.5);
            let rect_width = ratio * rect_height;

            let x_abs = x.abs().min(rect_width) / rect_width;
            let y_abs = y.abs().min(rect_height) / rect_height;
            (x_abs.copysign(x), y_abs.copysign(y))
        };

        // Convert to on-screen window co-ordinates
        let x = (x / 2.0 + 0.5) * vw + vx;
        let y = (y / 2.0 + 0.5) * vh + vy;

        let (old_x, old_y, old_pressed, _old_visible) =
            self.virtual_cursor_last.unwrap_or_default();

        let (x, y) = if let Some((smoothing_strength, sticky_radius)) =
            options.stabilize_virtual_cursor
        {
            let new_time = Instant::now();

            let (old_x_unsticky, old_y_unsticky, old_time) = self
                .virtual_cursor_last_unsticky
                .unwrap_or((0.0, 0.0, new_time));

            let delta_t = new_time.saturating_duration_since(old_time).as_secs_f32();

            // Apply a feedback-based smoothing with exponential decay, to try
            // to dampen shakiness in the stick movement.

            let smooth = |old: f32, new: f32| -> f32 {
                if smoothing_strength != 0.0 {
                    let lerp_factor = 1.0 - (0.5_f32).powf(delta_t * (1.0 / smoothing_strength));
                    old + (new - old) * lerp_factor
                } else {
                    new
                }
            };

            let new_x_unsticky = smooth(old_x_unsticky, x);
            let new_y_unsticky = smooth(old_y_unsticky, y);

            self.virtual_cursor_last_unsticky = Some((new_x_unsticky, new_y_unsticky, new_time));

            // Make the reported position "sticky" within a certain radius, i.e.
            // if the new position's distance from the old one is within the
            // radius, report no change in position.

            if (new_x_unsticky - old_x).hypot(new_y_unsticky - old_y) < sticky_radius {
                (old_x, old_y)
            } else {
                (new_x_unsticky, new_y_unsticky)
            }
        } else {
            (x, y)
        };

        self.virtual_cursor_last = Some((x, y, pressed, visible));

        (
            x,
            y,
            pressed,
            pressed != old_pressed,
            x != old_x || y != old_y,
        )
    }

    /// Get the summed X and Y positions and button state of the left or right
    /// analog stick of the game controllers. Each axis value is in the range
    /// [-1, 1].
    fn get_controller_stick(&self, options: &Options, left: bool) -> (f32, f32, bool) {
        fn convert_axis(axis: i16, deadzone: f32) -> f32 {
            assert!(deadzone >= 0.0);
            let axis = ((axis as f32) / (i16::MAX as f32)).clamp(-1.0, 1.0);
            let abs_axis = (axis.abs().max(deadzone) - deadzone) / (1.0 - deadzone);
            abs_axis.copysign(axis)
        }

        let (mut x, mut y) = (0.0, 0.0);
        let mut pressed = false;
        for controller in &self.controllers {
            use sdl2::controller::{Axis, Button};
            let (x_axis, y_axis, button1, button2) = if left {
                (
                    Axis::LeftX,
                    Axis::LeftY,
                    Button::LeftStick,
                    Button::LeftShoulder,
                )
            } else {
                (
                    Axis::RightX,
                    Axis::RightY,
                    Button::RightStick,
                    Button::RightShoulder,
                )
            };
            x += convert_axis(controller.axis(x_axis), options.deadzone);
            y += convert_axis(controller.axis(y_axis), options.deadzone);
            pressed |= controller.button(button1);
            pressed |= controller.button(button2);
        }
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));

        (x, y, pressed)
    }

    pub fn create_gl_context(&self, version: GLVersion) -> Result<GLContext, String> {
        let attr = self.video_ctx.gl_attr();
        match version {
            GLVersion::GLES11 => {
                attr.set_context_version(1, 1);
                attr.set_context_profile(sdl2::video::GLProfile::GLES);
            }
            GLVersion::GL21Compat => {
                attr.set_context_version(2, 1);
                attr.set_context_profile(sdl2::video::GLProfile::Compatibility);
            }
        }

        let gl_ctx = self.window.gl_create_context()?;

        Ok(GLContext(gl_ctx))
    }

    pub fn gl_get_proc_address(&self, procname: &str) -> *const std::ffi::c_void {
        // For some reason, rust-sdl2 uses *const (), but () is not meant to be
        // used for void pointees (just void results), so let's fix that.
        self.video_ctx.gl_get_proc_address(procname) as *const _
    }

    pub fn set_share_with_current_context(&self, value: bool) {
        self.video_ctx
            .gl_attr()
            .set_share_with_current_context(value)
    }

    pub unsafe fn make_gl_context_current(&self, gl_ctx: &GLContext) {
        self.window.gl_make_current(&gl_ctx.0).unwrap();
    }

    /// Make the internal OpenGL ES context (for splash screen and UI rendering)
    /// current.
    #[must_use]
    pub fn make_internal_gl_ctx_current<'win>(&'win mut self) -> Box<dyn GLES + 'win> {
        // The invariant is held up here - since the instance we return is
        // bound to the lifetime of window, it can't outlive the internal GL
        // context and can't outlive the window.
        let gl_ins = unsafe {
            self.internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |gl_ctx| self.window.gl_make_current(&gl_ctx.0).unwrap(),
                    &mut |s| self.video_ctx.gl_get_proc_address(s) as *const _,
                )
        };
        gl_ins
    }

    fn display_splash(&mut self) {
        assert!(self.splash_image.is_some());

        // OpenGL ES expects bottom-to-top row order for image data, but our
        // image data will be top-to-bottom. A reflection transform compensates.
        let matrix = self.rotation_matrix().multiply(&Matrix::y_flip());
        let (vx, vy, vw, vh) = self.viewport();
        let viewport = (vx, vy + self.viewport_y_offset(), vw, vh);

        let image = self.splash_image.as_ref().unwrap();

        unsafe {
            let mut gl_ctx = self
                .internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |gl_ctx| self.window.gl_make_current(&gl_ctx.0).unwrap(),
                    &mut |s| self.video_ctx.gl_get_proc_address(s) as *const _,
                );

            use crate::gles::gles11_raw as gles11; // constants only

            let mut texture = 0;
            gl_ctx.GenTextures(1, &mut texture);
            gl_ctx.BindTexture(gles11::TEXTURE_2D, texture);
            let (width, height) = image.dimensions();
            gl_ctx.TexImage2D(
                gles11::TEXTURE_2D,
                0,
                gles11::RGBA as _,
                width as _,
                height as _,
                0,
                gles11::RGBA,
                gles11::UNSIGNED_BYTE,
                image.pixels().as_ptr() as *const _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MIN_FILTER,
                gles11::LINEAR as _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MAG_FILTER,
                gles11::LINEAR as _,
            );

            present_frame(
                gl_ctx.as_mut(),
                viewport,
                matrix,
                /* virtual_cursor_visible_at: */ None,
            );

            gl_ctx.DeleteTextures(1, &texture);
        };

        self.window.gl_swap_window();

        // hold onto GL context so the image doesn't disappear, and hold
        // onto image so we can rotate later if necessary
    }

    /// iOS only: present an already-composited RGBA8 frame (origin bottom-left,
    /// as produced by glReadPixels) to the screen via CoreAnimation, bypassing
    /// the broken OpenGL ES / EAGL present path. The frame is vertically flipped
    /// into a fresh buffer and handed to the main thread, which wraps it in a
    /// CGImage and assigns it to an overlay CALayer's contents.
    #[cfg(target_os = "ios")]
    pub fn present_frame_to_calayer(&self, pixels: &[u8], w: u32, h: u32) {
        use std::os::raw::c_void;
        let (w, h) = (w as usize, h as usize);
        let n = w * h * 4;
        if n == 0 || pixels.len() < n {
            return;
        }
        extern "C" {
            static _dispatch_main_q: c_void;
            fn dispatch_async_f(
                queue: *const c_void,
                context: *mut c_void,
                work: extern "C" fn(*mut c_void),
            );
        }
        // The pixels are the guest's portrait (w x h, typically 320x480) offscreen
        // framebuffer, bottom-up (glReadPixels). CGImage wants top-to-bottom, so we
        // always flip vertically. When the emulated device is landscape we ALSO
        // rotate 90°, so the content displays upright and fills the rotated window.
        // We do this on the CPU (instead of via the SDL screen framebuffer) because
        // after an iOS-forced rotation SDL's renderbuffer stays portrait-sized while
        // the window is landscape — reading the offscreen + rotating here sidesteps
        // that desync entirely.
        let landscape = matches!(
            self.device_orientation,
            DeviceOrientation::LandscapeLeft | DeviceOrientation::LandscapeRight
        );
        // PortraitUpsideDown: the shared compositor produces content that the
        // desktop present path un-rotates via its rotation matrix; this iOS CPU
        // path only vertically flips, which leaves the result upside-down on
        // device. So for upside-down we rotate the would-be-portrait output 180°.
        let upside_down = matches!(self.device_orientation, DeviceOrientation::PortraitUpsideDown);
        unsafe {
            let buf = libc::malloc(n) as *mut u8;
            if buf.is_null() {
                return;
            }
            let (out_w, out_h) = if landscape {
                // Combined vertical-flip + 90° COUNTER-clockwise rotation. Output
                // is (h wide x w tall). CCW (not CW) so landscape content lands
                // right-side up — CW came out 180° upside-down.
                // out[oy][ox] = flipped[ox][w-1-oy] = pixels[h-1-ox][w-1-oy].
                let (ow, oh) = (h, w);
                for oy in 0..oh {
                    for ox in 0..ow {
                        let src = ((h - 1 - ox) * w + (w - 1 - oy)) * 4;
                        let dst = (oy * ow + ox) * 4;
                        std::ptr::copy_nonoverlapping(pixels.as_ptr().add(src), buf.add(dst), 4);
                    }
                }
                (ow, oh)
            } else if upside_down {
                // Rotate the portrait result 180° so an upside-down app renders
                // right-side up on a normally-held device. Empirically (vs the
                // pure-vflip portrait output, which came out inverted on iOS):
                //   out[y][x] = pixels[y][w-1-x]
                for y in 0..h {
                    for x in 0..w {
                        let src = (y * w + (w - 1 - x)) * 4;
                        let dst = (y * w + x) * 4;
                        std::ptr::copy_nonoverlapping(pixels.as_ptr().add(src), buf.add(dst), 4);
                    }
                }
                (w, h)
            } else {
                let row = w * 4;
                for y in 0..h {
                    let src_off = (h - 1 - y) * row;
                    std::ptr::copy_nonoverlapping(
                        pixels.as_ptr().add(src_off),
                        buf.add(y * row),
                        row,
                    );
                }
                (w, h)
            };
            let payload = Box::new(PresentPayload {
                buf: buf as *mut c_void,
                w: out_w,
                h: out_h,
            });
            let ctx = Box::into_raw(payload) as *mut c_void;
            dispatch_async_f(&_dispatch_main_q as *const c_void, ctx, present_on_main);
        }
    }

    /// Swap front-buffer and back-buffer so the result of OpenGL rendering is
    /// presented.
    pub fn swap_window(&self) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static PRESENT_COUNT: AtomicU64 = AtomicU64::new(0);
        let n = PRESENT_COUNT.fetch_add(1, Ordering::Relaxed);
        self.window.gl_swap_window();
        // Heartbeat: proves the present loop is actually running each frame, and
        // reports whether the window is shown with a non-zero drawable (i.e.
        // whether SDL's view is laid out). Rate-limited to avoid log spam.
        if n < 5 || n % 600 == 0 {
            let (dw, dh) = self.window.drawable_size();
            let flags = self.window.window_flags();
            log!(
                "[diag] present #{}: drawable={}x{} SHOWN={} MINIMIZED={} sdl_err={:?}",
                n,
                dw,
                dh,
                flags & 0x0000_0004 != 0,
                flags & 0x0000_0040 != 0,
                sdl2::get_error()
            );
            #[cfg(target_os = "ios")]
            unsafe {
                ios_dump_view_hierarchy("during-run-loop");
                ios_dump_orientation("during-run-loop");
            }
        }
    }

    /// The framebuffer object that represents the visible screen. This is 0 (the
    /// window's default framebuffer) on desktop, but a non-zero SDL-created FBO
    /// on iOS. Present final frames into this rather than hardcoding 0.
    pub fn gl_default_framebuffer(&self) -> u32 {
        self.gl_default_framebuffer
    }

    /// Consider the emulated device to be rotated to a particular orientation.
    ///
    /// On a PC or laptop, this will make the window be rotated so the app
    /// content appears upright. On a mobile device, this might do something
    /// else, because the user can physically rotate the screen.
    pub fn rotate_device(&mut self, new_orientation: DeviceOrientation) {
        assert!(self.on_main_stack);
        if new_orientation == self.device_orientation {
            return;
        }
        // If force_portrait is set (user chose "Portrait" in Quick Options),
        // ignore any game request to switch to landscape — the game stays in
        // the portrait letterboxed view. Useful for portrait-only apps and for
        // users who prefer holding the device upright.
        if self.force_portrait
            && matches!(
                new_orientation,
                DeviceOrientation::LandscapeLeft | DeviceOrientation::LandscapeRight
            )
        {
            return;
        }

        if !self.fullscreen && !Self::rotatable_fullscreen() {
            let (width, height) = if Self::rotatable_fullscreen() {
                set_sdl2_orientation(new_orientation);
                rotate_fullscreen_size(new_orientation, self.window.size())
            } else {
                size_for_orientation(self.device_family, new_orientation, self.scale_hack)
            };

            // macOS quirk: when resizing the window, the new framebuffer's size
            // is apparently max(new_size, old_size) in each dimension, but the
            // viewport is positioned wrong on the y axis for some reason, so we
            // need to apply an offset.
            // Recreating the OpenGL context was an alternative workaround, but
            // that apparently stops other OpenGL contexts drawing to the
            // framebuffer!
            #[cfg(target_os = "macos")]
            {
                let (_old_width, old_height) = self.window.size();
                self.max_height = self.max_height.max(old_height).max(height);
                self.viewport_y_offset = self.max_height - height;
            }

            self.window.set_size(width, height).unwrap();
        }

        if Self::rotatable_fullscreen() {
            set_sdl2_orientation(new_orientation);
            // Hack: from reading SDL2's source code, it seems that SDL2 will
            // only re-do the orientation when changing whether a window is
            // "resizeable" (can be rotated). You can't set the resizeable state
            // on a fullscreen window, so it must be temporarily stop being
            // fulscreen.
            // Apparently, doing this does result in resizing the window.
            self.window
                .set_fullscreen(sdl2::video::FullscreenType::Off)
                .unwrap();
            unsafe {
                let window_raw = self.window.raw();
                sdl2_sys::SDL_SetWindowResizable(window_raw, sdl2_sys::SDL_bool::SDL_FALSE);
                sdl2_sys::SDL_SetWindowResizable(window_raw, sdl2_sys::SDL_bool::SDL_TRUE);
            }
            self.window
                .set_fullscreen(sdl2::video::FullscreenType::True)
                .unwrap();

            // SDL updated the view controller's supportedInterfaceOrientations,
            // but iOS won't re-rotate the app until explicitly asked to. Without
            // this the app stays portrait (the game shows as a letterboxed
            // strip); with it the app rotates to match (fullscreen landscape).
            #[cfg(target_os = "ios")]
            ios_request_orientation_update();
        }

        self.device_orientation = new_orientation;

        if self.splash_image.is_some() {
            self.display_splash();
        }
    }

    pub fn device_family(&self) -> DeviceFamily {
        self.device_family
    }

    /// Returns the current device orientation
    pub fn current_rotation(&self) -> DeviceOrientation {
        self.device_orientation
    }

    /// Get the size in pixels of the window without rotation or scaling.
    ///
    /// The aspect ratio, scale and orientation reflect the guest app's view of
    /// the world.
    pub fn size_unrotated_unscaled(&self) -> (u32, u32) {
        size_for_orientation(
            self.device_family,
            DeviceOrientation::Portrait,
            NonZeroU32::new(1).unwrap(),
        )
    }

    /// Get the region of the on-screen window (x, y, width, height) used to
    /// display the app content.
    ///
    /// The aspect ratio of this region always reflects the guest app's view of
    /// the world, but the scale and orientation might not.
    pub fn viewport(&self) -> (u32, u32, u32, u32) {
        let (app_width, app_height) =
            size_for_orientation(self.device_family, self.device_orientation, self.scale_hack);
        if !self.fullscreen && !Self::rotatable_fullscreen() {
            return (0, 0, app_width, app_height);
        }

        let (screen_width, screen_height) = self.window.drawable_size();

        let app_aspect = app_width as f32 / app_height as f32;
        let screen_aspect = screen_width as f32 / screen_height as f32;
        let (scaled_width, scaled_height) = if app_aspect < screen_aspect {
            (
                (screen_height as f32 * app_aspect).round() as u32,
                screen_height,
            )
        } else {
            (
                screen_width,
                (screen_width as f32 / app_aspect).round() as u32,
            )
        };
        let x = (screen_width - scaled_width) / 2;
        let y = (screen_height - scaled_height) / 2;
        (x, y, scaled_width, scaled_height)
    }

    /// Special offset to add to y co-ordinates, only when drawing to screen.
    pub fn viewport_y_offset(&self) -> u32 {
        #[cfg(target_os = "macos")]
        return self.viewport_y_offset;
        #[cfg(not(target_os = "macos"))]
        return 0;
    }

    /// Transformation matrix for transforming between the window's co-ordinate
    /// space and the app's original co-ordinate space when rotation is in use
    /// (see [Self::rotate_device]). This returns a matrix appropriate for
    /// rotating texture co-ordinates to display the image in the window; when
    /// rotating input co-ordinates, invert the matrix.
    pub fn rotation_matrix(&self) -> Matrix<2> {
        // [jc3] JC3 renders its framebuffer already in landscape, so applying the
        // normal portrait→landscape rotation over-rotates it 90°. Present it
        // un-rotated. (Only the EAGL direct-present path uses this for JC3; the
        // compositor is skipped.)
        if crate::mem::JC3_DIRECT_EAGL_PRESENT.load(std::sync::atomic::Ordering::Relaxed) {
            return Matrix::identity();
        }
        match self.device_orientation {
            DeviceOrientation::Portrait => Matrix::identity(),
            DeviceOrientation::PortraitUpsideDown => Matrix::z_rotation(PI),
            DeviceOrientation::LandscapeLeft => Matrix::z_rotation(-FRAC_PI_2),
            DeviceOrientation::LandscapeRight => Matrix::z_rotation(FRAC_PI_2),
        }
    }

    pub fn is_screen_saver_enabled(&self) -> bool {
        self.video_ctx.is_screen_saver_enabled()
    }
    pub fn set_screen_saver_enabled(&mut self, enabled: bool) {
        assert!(self.on_main_stack);
        match enabled {
            true => self.video_ctx.enable_screen_saver(),
            false => self.video_ctx.disable_screen_saver(),
        }
    }

    pub fn start_text_input(&self) {
        assert!(self.on_main_stack);
        unsafe {
            sdl2_sys::SDL_StartTextInput();
        }
    }
    pub fn stop_text_input(&self) {
        assert!(self.on_main_stack);
        unsafe {
            sdl2_sys::SDL_StopTextInput();
        }
    }

    pub fn on_main_stack(&self) -> bool {
        self.on_main_stack
    }
}

pub fn open_url(env: &mut Environment, url: &str) -> Result<(), String> {
    env.on_parent_stack_in_coroutine(|_, _| sdl2::url::open_url(url).map_err(|e| e.to_string()))
}

/// Show an SDL messagebox for an error (typically after a panic).
///
/// The window argument allows for passing in the parent window for the
/// messagebox, which is not required but should be done if possible.
pub fn show_error_messagebox(window: Option<&Window>, error_message: &str) {
    assert!(window.is_none_or(|win| win.on_main_stack));
    use sdl2::messagebox;
    let mbox = [
        messagebox::ButtonData {
            flags: messagebox::MessageBoxButtonFlag::NOTHING,
            button_id: 0,
            text: "Open touchHLE directory",
        },
        messagebox::ButtonData {
            flags: messagebox::MessageBoxButtonFlag::NOTHING,
            button_id: 1,
            text: "Close",
        },
    ];

    let Ok(clicked_button) = messagebox::show_message_box(
        messagebox::MessageBoxFlag::ERROR,
        &mbox,
        "touchHLE crashed!",
        &format!("touchHLE crashed with the following error: {error_message}"),
        window.map(|win| &win.window),
        None,
    ) else {
        panic!("Failed to show message box!");
    };

    match clicked_button {
        messagebox::ClickedButton::CloseButton => {}
        messagebox::ClickedButton::CustomButton(button) => {
            match button.button_id {
                // Open data directory (contains log file on android)
                0 => match crate::paths::url_for_opening_user_data_dir() {
                    Ok(url) => {
                        if let Err(e) = sdl2::url::open_url(&url).map_err(|e| e.to_string()) {
                            echo!("Couldn't open file manager at {:?}: {}", url, e);
                        } else {
                            echo!("Opened file manager at {:?}, exiting.", url);
                        }
                    }
                    Err(e) => echo!("Couldn't open file manager: {}", e),
                },
                // Close
                1 => {}
                _ => unreachable!(),
            }
        }
    }
}

/// Get current battery state from SDL2.
///
/// Returns:
/// - pct: i32 - percentage of battery remaining.
/// - status: [BatteryState] - the current status of the battery
///   (unplugged, charging, full, etc.)
pub fn get_battery_status() -> (i32, BatteryState) {
    let mut pct = 0;
    // Unfortunately, Rust-SDL2 does not expose this function yet.
    // iPhoneOS does not measure the battery in seconds remaining,
    // so we discard this argument.
    let status = unsafe { sdl2_sys::SDL_GetPowerInfo(null_mut(), &mut pct) };
    (
        pct,
        match status {
            SDL_PowerState::SDL_POWERSTATE_UNKNOWN => BatteryState::Unknown,
            SDL_PowerState::SDL_POWERSTATE_ON_BATTERY => BatteryState::OnBattery,
            SDL_PowerState::SDL_POWERSTATE_NO_BATTERY => BatteryState::NoBattery,
            SDL_PowerState::SDL_POWERSTATE_CHARGING => BatteryState::Charging,
            SDL_PowerState::SDL_POWERSTATE_CHARGED => BatteryState::Full,
        },
    )
}

pub fn get_preferred_language_codes(env: &mut Environment) -> Vec<String> {
    env.on_parent_stack_in_coroutine(|_, _| {
        sdl2::locale::get_preferred_locales()
            .map(|loc| loc.lang)
            .collect()
    })
}

pub fn get_preferred_country_codes(env: &mut Environment) -> Vec<String> {
    env.on_parent_stack_in_coroutine(|_, _| {
        sdl2::locale::get_preferred_locales()
            .filter_map(|loc| loc.country)
            .collect()
    })
}
