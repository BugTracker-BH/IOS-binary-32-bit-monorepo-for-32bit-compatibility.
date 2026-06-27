/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UINavigationBar`.
//!
//! Minimal stub so apps (e.g. JellyCar 3, via its ad UI) that reference
//! `UINavigationBar` don't crash with "Class UINavigationBar is unimplemented".
//! It inherits `UIView` (including the nib-decoding `initWithCoder:`);
//! navigation-bar-specific setters are accepted as no-ops so creation and layout
//! proceed. The bar renders as a plain (empty) view.

use crate::objc::{id, nil, objc_classes, ClassExports};

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

};
