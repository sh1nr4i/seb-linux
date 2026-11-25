use base64::Engine;
use seb_config::settings::ClipboardPolicy;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const MAX_CLIPBOARD_B64_BYTES: usize = 8 * 1024 * 1024;
const MAX_CLIPBOARD_ID_LEN: usize = 128;

/// Clipboard isolation matching SEB security rules without breaking WebKitGTK text selection:
/// - When `policy != ClipboardPolicy::Allow`:
///   - Clears OS `CLIPBOARD` whenever non-empty (periodic check, startup, shutdown).
///   - Clears `PRIMARY` selection on startup, policy change, focus loss, and shutdown — NEVER on a
///     50ms loop while SEB is active, because taking `SELECTION_PRIMARY` away from WebKitGTK
///     causes WebKitGTK to immediately collapse the active text selection highlight.
/// - When `policy == ClipboardPolicy::Isolated`: stores an in-memory session-wide
///   base64-encoded clipboard synced across all SEB panes/frames.
pub struct IsolatedClipboard {
    policy: Cell<ClipboardPolicy>,
    last_id: RefCell<String>,
    last_b64: RefCell<String>,
}

impl IsolatedClipboard {
    pub fn new(policy: ClipboardPolicy) -> Rc<Self> {
        let cb = Rc::new(Self {
            policy: Cell::new(policy),
            last_id: RefCell::new(String::new()),
            last_b64: RefCell::new(String::new()),
        });
        if policy != ClipboardPolicy::Allow {
            clear_gtk_clipboards();
        }
        cb
    }

    pub fn set_policy(&self, policy: ClipboardPolicy) {
        self.policy.set(policy);
        self.last_id.borrow_mut().clear();
        self.last_b64.borrow_mut().clear();
        if policy != ClipboardPolicy::Allow {
            clear_gtk_clipboards();
        }
    }

    pub fn policy(&self) -> ClipboardPolicy {
        self.policy.get()
    }

    /// Periodic tick: wipes OS `CLIPBOARD` if non-empty without stealing `SELECTION_PRIMARY`
    /// while the user is actively selecting text inside SEB.
    pub fn clear_os_clipboards_if_restricted(&self) {
        if self.policy.get() == ClipboardPolicy::Allow {
            return;
        }
        clear_os_clipboard_only();
    }

    /// Validates and records a clipboard update from a frame's `Clipboard.js`,
    /// returning `(id, b64)` to broadcast to all open panes if accepted.
    pub fn accept_js_update(&self, id: &str, encoded_b64: &str) -> Option<(String, String)> {
        if self.policy.get() != ClipboardPolicy::Isolated {
            return None;
        }
        if id.is_empty()
            || id.len() > MAX_CLIPBOARD_ID_LEN
            || encoded_b64.len() > MAX_CLIPBOARD_B64_BYTES
        {
            return None;
        }
        if !encoded_b64.is_empty()
            && base64::engine::general_purpose::STANDARD
                .decode(encoded_b64)
                .is_err()
        {
            return None;
        }
        *self.last_id.borrow_mut() = id.to_string();
        *self.last_b64.borrow_mut() = encoded_b64.to_string();
        self.clear_os_clipboards_if_restricted();
        Some((id.to_string(), encoded_b64.to_string()))
    }

    pub fn current_state(&self) -> (String, String) {
        (self.last_id.borrow().clone(), self.last_b64.borrow().clone())
    }
}

pub fn clear_os_clipboard_only() {
    let clip = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
    if clip.wait_for_targets().is_some() {
        clip.set_text("");
        clip.clear();
    }
}

pub fn clear_gtk_clipboards() {
    clear_os_clipboard_only();
    let prim = gtk::Clipboard::get(&gdk::SELECTION_PRIMARY);
    if prim.wait_for_targets().is_some() {
        prim.set_text("");
        prim.clear();
    }
}
