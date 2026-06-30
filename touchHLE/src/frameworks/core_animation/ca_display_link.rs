/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CADisplayLink`

use crate::frameworks::foundation::ns_run_loop::NSRunLoopMode;
use crate::frameworks::foundation::ns_timer::set_time_interval;
use crate::frameworks::foundation::NSInteger;
use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, nil, objc_classes, release, retain, ClassExports,
    HostObject, NSZonePtr, SEL,
};

#[derive(Default)]
struct CADisplayLinkHostObject {
    target: id,
    selector: Option<SEL>,
    /// Weak reference. The timer retains the display link (as its target),
    /// so the timer necessarily outlives the display link. After `invalidate`,
    /// this pointer must not be used.
    ns_timer: id,
    paused: bool,
}
impl HostObject for CADisplayLinkHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation CADisplayLink: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(this, Box::new(CADisplayLinkHostObject::default()), &mut env.mem)
}

+ (id)displayLinkWithTarget:(id)target selector:(SEL)sel {
    let display_link: id = msg![env; this new];
    // Because timer will pass itself as a second arg in ns_timer:handle_timer,
    // we need to use a re-direction: first fire the timer on the display link,
    // then call the original selector, passing the link as a second argument!
    let redirect_sel: SEL = env.objc.lookup_selector("_touchHLE_displayLinkTimerDidFire:").unwrap();
    let ns_timer = msg_class![env; NSTimer timerWithTimeInterval:(1.0/60.0)
                     target:display_link
                   selector:redirect_sel
                   userInfo:nil
                    repeats:true];
    retain(env, target);
    let host_object = env.objc.borrow_mut::<CADisplayLinkHostObject>(display_link);
    host_object.target = target;
    host_object.selector = Some(sel);
    host_object.ns_timer = ns_timer;
    log_dbg!("[CADisplayLink displayLinkWithTarget:{:?} selector:{}] => {:?}", target, sel.as_str(&env.mem), display_link);
    autorelease(env, display_link)
}

- (bool)isPaused {
    env.objc.borrow::<CADisplayLinkHostObject>(this).paused
}
- (())setPaused:(bool)paused {
    env.objc.borrow_mut::<CADisplayLinkHostObject>(this).paused = paused;
}

- (())setFrameInterval:(NSInteger)frameInterval {
    log_dbg!("[(CADisplayLink*){:?} setFrameInterval:{}]", this, frameInterval);
    assert!(frameInterval >= 1);
    let interval = frameInterval as f64 / 60.0;
    let ns_timer = env.objc.borrow::<CADisplayLinkHostObject>(this).ns_timer;
    set_time_interval(env, ns_timer, interval);
}

- (())addToRunLoop:(id)run_loop forMode:(NSRunLoopMode)mode {
    log_dbg!("[(CADisplayLink*){:?} addToRunLoop:{:?} forMode:{:?}]", this, run_loop, mode);
    let ns_timer = env.objc.borrow::<CADisplayLinkHostObject>(this).ns_timer;
    () = msg![env; run_loop addTimer:ns_timer forMode:mode];
}

- (())invalidate {
    log_dbg!("[(CADisplayLink*){:?} invalidate]", this);
    let ns_timer = env.objc.borrow::<CADisplayLinkHostObject>(this).ns_timer;
    () = msg![env; ns_timer invalidate];
}

- (())dealloc {
    let &CADisplayLinkHostObject { target, .. } = env.objc.borrow(this);
    release(env, target);
    env.objc.dealloc_object(this, &mut env.mem);
}

- (())_touchHLE_displayLinkTimerDidFire:(id)timer { // NSTimer *
    let &CADisplayLinkHostObject {
        target,
        selector,
        ns_timer,
        paused,
        ..
    } = env.objc.borrow::<CADisplayLinkHostObject>(this);
    assert_eq!(ns_timer, timer);
    if paused {
        // This could be improved, as we're still running the timer,
        // but just not passing the actual call.
        return;
    }
    // One-shot: on the very first fire, trigger layoutSubviews on the key
    // window's view hierarchy. This is how JellyCar 3's EAGLView gets its
    // createFramebuffer called — it needs to happen after game init is done
    // (not during addSubview, which is too early and crashes JC1). Since JC1
    // doesn't use CADisplayLink, this path is JC1-safe. We use ObjC messages
    // (not the host `views` vec) because EAGLView is a guest-defined class.
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static LAYOUT_DONE: AtomicBool = AtomicBool::new(false);
        if !LAYOUT_DONE.swap(true, Ordering::Relaxed) {
            let app: id = msg_class![env; UIApplication sharedApplication];
            let window: id = msg![env; app keyWindow];
            if window != nil {
                // Get subviews and call layoutSubviews on each (including EAGLView)
                let subviews: id = msg![env; window subviews];
                let count: u32 = msg![env; subviews count];
                for i in 0..count {
                    let subview: id = msg![env; subviews objectAtIndex:i];
                    () = msg![env; subview layoutSubviews];
                    // Also check one level deeper (EAGLView may be a subview of the VC's view)
                    let inner_subviews: id = msg![env; subview subviews];
                    let inner_count: u32 = msg![env; inner_subviews count];
                    for j in 0..inner_count {
                        let inner: id = msg![env; inner_subviews objectAtIndex:j];
                        () = msg![env; inner layoutSubviews];
                    }
                }
            }
        }
    }
    // Signature is `- (void) selector:(CADisplayLink *)sender;`
    () = msg_send(env, (target, selector.unwrap(), this));
}

@end

};
