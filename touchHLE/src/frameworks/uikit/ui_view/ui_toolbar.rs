/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIToolbar`.
//!
//! Minimal stub so apps (e.g. JellyCar) that reference `UIToolbar` in their nib
//! don't crash with "Missing implementation for class UIToolbar!". It inherits
//! `UIView`'s behaviour (including the nib-decoding `initWithCoder:`); the
//! toolbar-specific properties are accepted as no-ops so nib loading and layout
//! proceed. The bar renders as a plain (empty) view.

use crate::objc::{id, objc_classes, ClassExports};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIToolbar: UIView

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
- (())setItems:(id)_items {
    // TODO
}
- (())setItems:(id)_items animated:(bool)_animated {
    // TODO
}

@end

};
