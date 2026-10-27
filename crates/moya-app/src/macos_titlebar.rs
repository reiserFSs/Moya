//! The toolbar is drawn into the (hidden) title bar area. AppKit keeps the traffic-light buttons
//! at their 28-pt title-bar position, so in a taller toolbar they sit too high and too close to
//! the left edge. Resize the title-bar container to the toolbar height and center the buttons in
//! it, like a unified-toolbar window. AppKit lays them out again on resize and full-screen
//! changes, so this runs every frame and only moves anything when the layout differs.
use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSPoint, NSRect};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Left edge of the close button, and distance between button origins, in points.
const LEADING: f64 = 14.0;
const SPACING: f64 = 20.0;
/// Right edge of the zoom button: where toolbar content may start.
pub const BUTTONS_END: f32 = (LEADING + 2.0 * SPACING + 14.0) as f32;

pub fn place_traffic_lights(window: &impl HasWindowHandle, toolbar_height: f32) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };
    let height = f64::from(toolbar_height);
    // SAFETY: the view pointer comes from the live window this frame belongs to; all calls are
    // standard NSView/NSWindow messages with matching argument and return encodings.
    unsafe {
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() { return; }
        // Full screen moves the buttons into a separate, system-managed toolbar window.
        let style: usize = msg_send![window, styleMask];
        if style & (1 << 14) != 0 { return; }
        let close: *mut AnyObject = msg_send![window, standardWindowButton: 0usize];
        if close.is_null() { return; }
        let titlebar: *mut AnyObject = msg_send![close, superview];
        let container: *mut AnyObject = if titlebar.is_null() { return } else { msg_send![titlebar, superview] };
        if container.is_null() { return; }
        let window_frame: NSRect = msg_send![window, frame];
        let mut frame: NSRect = msg_send![container, frame];
        let target_y = window_frame.size.height - height;
        if (frame.size.height - height).abs() > 0.5 || (frame.origin.y - target_y).abs() > 0.5 {
            frame.size.height = height;
            frame.origin.y = target_y;
            let _: () = msg_send![container, setFrame: frame];
        }
        for (index, kind) in [0usize, 1, 2].into_iter().enumerate() {
            let button: *mut AnyObject = msg_send![window, standardWindowButton: kind];
            if button.is_null() { continue; }
            let button_frame: NSRect = msg_send![button, frame];
            let origin = NSPoint::new(LEADING + SPACING * index as f64, ((height - button_frame.size.height) / 2.0).round());
            if (button_frame.origin.x - origin.x).abs() > 0.5 || (button_frame.origin.y - origin.y).abs() > 0.5 {
                let _: () = msg_send![button, setFrameOrigin: origin];
            }
        }
    }
}
