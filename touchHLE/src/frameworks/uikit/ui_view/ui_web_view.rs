/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIWebView`.

use crate::frameworks::foundation::ns_string::to_rust_string;
use crate::msg;
use crate::objc::{id, msg_super, nil, objc_classes, ClassExports};
use std::borrow::Cow;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIWebView: UIView

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    // UIWebView is not implemented (no web engine in touchHLE). Initialize as a
    // plain empty UIView via the superclass so apps that load one from a nib —
    // e.g. JellyCar 3's full-screen web view (news/ads/"more games") opened from
    // the menu — get a valid, inert view instead of crashing.
    let this: id = msg_super![env; this initWithCoder:coder];
    log!(
        "TODO: [(UIWebView*){:?} initWithCoder:] stubbed as empty UIView (web view unimplemented)",
        this
    );
    this
}

- (())setScalesPageToFit:(bool)_scales {
    // TODO
}
- (())setDelegate:(id)_delegate {
    // TODO
}
- (())loadRequest:(id)request { // NSURLRequest*
    let url_string = if request != nil {
        let url = msg![env; request URL];
        let url_desc = msg![env; url description];
        to_rust_string(env, url_desc)
    } else {
        Cow::default()
    };
    log!("TODO: [(UIWebView*) {:?} loadRequest:{:?} ({})]", this, request, url_string);
}

@end

};
