/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITableView`.
//!
//! Minimal stub so apps (e.g. JellyCar 2) that reference `UITableView` in a nib
//! or at runtime don't crash with "Missing implementation for class
//! UITableView!". It inherits `UIScrollView` (the real superclass), including the
//! nib-decoding `initWithCoder:`; table-specific setters are accepted as no-ops
//! so nib loading and layout proceed. The table renders empty (no rows) rather
//! than crashing the whole emulator.

use crate::objc::{id, nil, objc_classes, ClassExports};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITableView: UIScrollView

- (())setDataSource:(id)_data_source {
    // TODO
}
- (())setRowHeight:(f32)_height {
    // TODO
}
- (())setSectionHeaderHeight:(f32)_height {
    // TODO
}
- (())setSectionFooterHeight:(f32)_height {
    // TODO
}
- (())setSeparatorStyle:(i64)_style {
    // TODO
}
- (())setSeparatorColor:(id)_color {
    // TODO
}
- (())setBackgroundColor:(id)_color {
    // TODO
}
- (())setAllowsSelection:(bool)_allows {
    // TODO
}
- (())setEditing:(bool)_editing {
    // TODO
}
- (())setEditing:(bool)_editing animated:(bool)_animated {
    // TODO
}
- (())reloadData {
    // TODO
}
- (id)dequeueReusableCellWithIdentifier:(id)_identifier {
    nil
}

@end

};
