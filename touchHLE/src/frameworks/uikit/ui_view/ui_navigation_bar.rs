/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UINavigationBar` and its companion model classes `UINavigationItem` and
//! `UIBarButtonItem`.
//!
//! Minimal stubs so apps (e.g. JellyCar 3, via its ad UI) that build a
//! navigation bar don't crash with "Class … is unimplemented". `UINavigationBar`
//! inherits `UIView`; the item classes inherit `NSObject`. All the
//! navigation-specific setters/initialisers are accepted as no-ops (the inits
//! just return the allocated object) so creation and layout proceed.

use crate::objc::{id, nil, objc_classes, ClassExports, SEL};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UINavigationBar: UIView

- (())setBarStyle:(i64)_style {
    // TODO
}
- (())setTranslucent:(bool)_translucent {
    // TODO
}
- (())setTintColor:(id)_color {
    // TODO
}
- (())setBarTintColor:(id)_color {
    // TODO
}
- (())setDelegate:(id)_delegate {
    // TODO
}
- (())setItems:(id)_items {
    // TODO
}
- (())setItems:(id)_items animated:(bool)_animated {
    // TODO
}
- (())pushNavigationItem:(id)_item animated:(bool)_animated {
    // TODO
}
- (id)popNavigationItemAnimated:(bool)_animated {
    nil
}

@end

@implementation UINavigationItem: NSObject

- (id)initWithTitle:(id)_title {
    this
}
- (())setTitle:(id)_title {}
- (())setTitleView:(id)_view {}
- (())setPrompt:(id)_prompt {}
- (())setHidesBackButton:(bool)_hidden {}
- (())setHidesBackButton:(bool)_hidden animated:(bool)_animated {}
- (())setBackBarButtonItem:(id)_item {}
- (())setLeftBarButtonItem:(id)_item {}
- (())setLeftBarButtonItem:(id)_item animated:(bool)_animated {}
- (())setRightBarButtonItem:(id)_item {}
- (())setRightBarButtonItem:(id)_item animated:(bool)_animated {}

@end

@implementation UIBarButtonItem: NSObject

- (id)initWithTitle:(id)_title style:(i64)_style target:(id)_target action:(SEL)_action {
    this
}
- (id)initWithBarButtonSystemItem:(i64)_item target:(id)_target action:(SEL)_action {
    this
}
- (id)initWithImage:(id)_image style:(i64)_style target:(id)_target action:(SEL)_action {
    this
}
- (id)initWithCustomView:(id)_view {
    this
}
- (())setTarget:(id)_target {}
- (())setAction:(SEL)_action {}
- (())setStyle:(i64)_style {}
- (())setEnabled:(bool)_enabled {}
- (())setTitle:(id)_title {}
- (())setWidth:(f32)_width {}
- (())setTintColor:(id)_color {}

@end

};
