use std::cell::RefCell;
use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use objc2::rc::Retained;
use objc2::{declare_class, msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
use objc2_foundation::{
    MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSNotification,
    NSNotificationCenter, NSObject,
};

const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

struct OpenQueue {
    sender: Sender<PathBuf>,
    context: RefCell<Option<egui::Context>>,
}

impl OpenQueue {
    fn forward(&self, path: PathBuf) {
        if self.sender.send(path).is_ok() {
            // Release the borrow before invoking egui's repaint callback.
            let context = self.context.borrow().clone();
            if let Some(context) = context {
                context.request_repaint();
            }
        }
    }
}

declare_class!(
    struct OpenDocumentsHandler;

    // NSObject has no subclassing requirements. Apple Events and the egui
    // context are accessed only on the application's main thread.
    unsafe impl ClassType for OpenDocumentsHandler {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "MoyaOpenDocumentsHandler";
    }

    impl DeclaredClass for OpenDocumentsHandler {
        type Ivars = OpenQueue;
    }

    unsafe impl OpenDocumentsHandler {
        #[method(applicationWillFinishLaunching:)]
        fn application_will_finish_launching(&self, _notification: &NSNotification) {
            self.register();
        }

        #[method(handleOpenDocuments:withReplyEvent:)]
        fn handle_open_documents(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            // These selectors are absent from objc2-foundation 0.2's bindings.
            let documents: Option<Retained<NSAppleEventDescriptor>> = unsafe {
                msg_send_id![event, paramDescriptorForKeyword: DIRECT_OBJECT]
            };
            let Some(documents) = documents else {
                return;
            };
            unsafe {
                // Apple Event descriptor lists are one-indexed.
                for index in 1..=documents.numberOfItems() {
                    let Some(document) = documents.descriptorAtIndex(index) else {
                        continue;
                    };
                    let Some(url) = document.fileURLValue() else {
                        continue;
                    };
                    if !url.isFileURL() {
                        continue;
                    }
                    let bytes = CStr::from_ptr(url.fileSystemRepresentation().as_ptr());
                    self.ivars()
                        .forward(PathBuf::from(OsStr::from_bytes(bytes.to_bytes())));
                }
            }
        }
    }
);

impl OpenDocumentsHandler {
    fn register(&self) {
        unsafe {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            let _: () = msg_send![
                &*manager,
                setEventHandler: self,
                andSelector: sel!(handleOpenDocuments:withReplyEvent:),
                forEventClass: CORE_EVENT_CLASS,
                andEventID: OPEN_DOCUMENTS
            ];
        }
    }
}

/// Keeps the Apple Event target alive and unregisters it before releasing it.
pub struct OpenDocuments {
    handler: Retained<OpenDocumentsHandler>,
    manager: Retained<NSAppleEventManager>,
    notifications: Retained<NSNotificationCenter>,
}

impl OpenDocuments {
    pub fn set_context(&self, context: egui::Context) {
        *self.handler.ivars().context.borrow_mut() = Some(context.clone());
        // Documents may have arrived before the app/context existed.
        context.request_repaint();
    }
}

impl Drop for OpenDocuments {
    fn drop(&mut self) {
        unsafe {
            self.notifications.removeObserver(&*self.handler);
            let _: () = msg_send![
                &*self.manager,
                removeEventHandlerForEventClass: CORE_EVENT_CLASS,
                andEventID: OPEN_DOCUMENTS
            ];
        }
    }
}

/// Call on the main thread before run_native, and retain the guard in the app.
pub fn install() -> (OpenDocuments, Receiver<PathBuf>) {
    let main_thread = MainThreadMarker::new().expect("document events require the main thread");
    // Initialize AppKit before registering; finishLaunching registers again.
    let _application = NSApplication::sharedApplication(main_thread);
    let (sender, receiver) = mpsc::channel();
    let allocated = main_thread.alloc::<OpenDocumentsHandler>().set_ivars(OpenQueue {
        sender,
        context: RefCell::new(None),
    });
    unsafe {
        let handler: Retained<OpenDocumentsHandler> = msg_send_id![super(allocated), init];
        let manager = NSAppleEventManager::sharedAppleEventManager();
        let notifications = NSNotificationCenter::defaultCenter();
        // AppKit can replace Apple Event handlers during finishLaunching.
        // Reinstall at its documented launch notification, before open events.
        notifications.addObserver_selector_name_object(
            &*handler,
            sel!(applicationWillFinishLaunching:),
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
        );
        handler.register();
        (OpenDocuments { handler, manager, notifications }, receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_non_utf8_paths_before_context_exists() {
        let (sender, receiver) = mpsc::channel();
        let queue = OpenQueue {
            sender,
            context: RefCell::new(None),
        };
        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/document-\xff.moya"));
        queue.forward(path.clone());
        assert_eq!(receiver.try_recv().unwrap(), path);
    }
}
