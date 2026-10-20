//! wgpu leaves the window's `CAMetalLayer` without a color space for sRGB surfaces, and Core
//! Animation then performs no color matching: on wide-gamut (Display P3) screens Moya's sRGB
//! pixels look oversaturated next to color-managed apps such as Photoshop. Tag the layer as
//! sRGB. wgpu clears it again whenever it reconfigures the surface (e.g. on resize), so this
//! is called every frame and only acts when the tag is missing.
use std::{ffi::c_void, sync::LazyLock};

use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[repr(C)]
struct CGColorSpace {
    _private: [u8; 0],
}
// SAFETY: matches the Objective-C type encoding of `CGColorSpaceRef`.
unsafe impl RefEncode for CGColorSpace {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut CGColorSpace;
}

/// Created once and intentionally never released (lives for the whole process).
static SRGB: LazyLock<usize> = LazyLock::new(|| unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) } as usize);

pub fn tag_srgb(window: &impl HasWindowHandle) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };
    let Some(metal) = AnyClass::get("CAMetalLayer") else { return };
    // SAFETY: the view pointer comes from the live window this frame belongs to; all calls
    // are standard CALayer/NSArray messages with matching type encodings.
    unsafe {
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        let root: *mut AnyObject = msg_send![view, layer];
        if root.is_null() { return; }
        tag(root, metal);
        // raw-window-metal inserts the CAMetalLayer as a sublayer of the view's layer.
        let sublayers: *mut AnyObject = msg_send![root, sublayers];
        if sublayers.is_null() { return; }
        let count: usize = msg_send![sublayers, count];
        for index in 0..count {
            let layer: *mut AnyObject = msg_send![sublayers, objectAtIndex: index];
            tag(layer, metal);
        }
    }
}

unsafe fn tag(layer: *mut AnyObject, metal: &AnyClass) {
    unsafe {
        let is_metal: Bool = msg_send![layer, isKindOfClass: metal];
        if !is_metal.as_bool() { return; }
        let current: *mut CGColorSpace = msg_send![layer, colorspace];
        let srgb = *SRGB as *mut CGColorSpace;
        if current.is_null() && !srgb.is_null() {
            let _: () = msg_send![layer, setColorspace: srgb];
        }
    }
}
