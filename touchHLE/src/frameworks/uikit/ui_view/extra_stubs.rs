/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Minimal stubs for assorted classes that some apps (e.g. JellyCar 3) reference
//! but touchHLE doesn't implement, so they instantiate as empty objects instead
//! of crashing with "Class ... is unimplemented". Grouped here for convenience;
//! each subclasses an already-implemented base. (A few aren't strictly UIKit —
//! `NSInvocationOperation` is Foundation, `GKAchievementViewController` is
//! GameKit — but registering them here is harmless; classes are linked by name.)

use crate::objc::{id, nil, objc_classes, ClassExports, SEL};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// iAd banner. There is no ad network in touchHLE, so this never loads an ad.
@implementation ADBannerView: UIView
- (())setDelegate:(id)_delegate {}
- (())setRequiredContentSizeIdentifiers:(id)_ids {}
- (())setCurrentContentSizeIdentifier:(id)_identifier {}
- (bool)isBannerLoaded { false }
@end

@implementation UITableViewCell: UIView
- (id)initWithStyle:(i64)_style reuseIdentifier:(id)_reuse_identifier {
    this
}
- (id)contentView { this }
- (id)reuseIdentifier { nil }
- (id)textLabel { nil }
- (id)detailTextLabel { nil }
- (id)imageView { nil }
- (())setAccessoryType:(i64)_accessory_type {}
- (())setSelectionStyle:(i64)_selection_style {}
- (())prepareForReuse {}
@end

@implementation UISearchBar: UIView
- (())setDelegate:(id)_delegate {}
- (())setPlaceholder:(id)_placeholder {}
- (())setText:(id)_text {}
- (())setShowsCancelButton:(bool)_shows {}
- (id)text { nil }
@end

@implementation UIPageControl: UIView
- (())setNumberOfPages:(i64)_number_of_pages {}
- (())setCurrentPage:(i64)_current_page {}
- (i64)currentPage { 0 }
- (())setHidesForSinglePage:(bool)_hides {}
@end

@implementation GKAchievementViewController: UIViewController
- (())setAchievementDelegate:(id)_delegate {}
@end

@implementation NSInvocationOperation: NSObject
- (id)initWithTarget:(id)_target selector:(SEL)_sel object:(id)_object {
    this
}
@end

// iSimulate dev-tool SDK (streams accelerometer/GPS/multitouch from a device
// into the iOS Simulator). Useless in touchHLE and referenced by name in a nib,
// so we register an empty stub whose factory methods return nil — the tool stays
// disabled and the game proceeds.
@implementation iSimulate: NSObject
+ (id)sharedInstance {
    nil
}
+ (id)sharedInstanceWithMainView:(id)_view {
    nil
}
+ (id)sharedInstanceWithApplication:(id)_application {
    nil
}
- (())run {}
- (())start {}
- (())stop {}
@end

};
