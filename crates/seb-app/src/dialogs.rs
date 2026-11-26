use crate::assets;
use gtk::prelude::*;
use seb_server::ExamInfo;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

thread_local! {
    static MAIN_OVERLAY: RefCell<Option<glib::WeakRef<gtk::Overlay>>> = const { RefCell::new(None) };
    static MODAL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub fn register_main_overlay(overlay: &gtk::Overlay) {
    let weak = overlay.downgrade();
    MAIN_OVERLAY.with(|slot| *slot.borrow_mut() = Some(weak));
}

/// Forces frameless Client-Side Decoration (CSD) using a hidden 0px custom titlebar so KDE
/// KWin / Wayland (`org_kde_kwin_server_decoration` mode 1) and X11 WMs never attach dark SSD
/// titlebars (`[W ... ●●●]`) around splash, startup, or dialog windows.
pub fn make_window_frameless<W: IsA<gtk::Window>>(window: &W) {
    let empty_tb = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    empty_tb.set_no_show_all(true);
    empty_tb.hide();
    window.set_titlebar(Some(&empty_tb));
    window.set_decorated(true);
}

pub fn is_modal_active() -> bool {
    MODAL_DEPTH.with(|d| d.get() > 0)
}

/// Marks an external GTK modal dialog (such as `PrintOperation::run_dialog`) as active
/// so AntiCheat focus-loss monitoring does not warn while the user interacts with it.
pub fn with_external_modal<R, F: FnOnce() -> R>(f: F) -> R {
    let _guard = ModalDepthGuard::enter();
    f()
}

struct ModalDepthGuard {
    depth: usize,
}
impl ModalDepthGuard {
    fn enter() -> Self {
        let depth = MODAL_DEPTH.with(|d| {
            let next = d.get() + 1;
            d.set(next);
            next
        });
        Self { depth }
    }

    fn depth(&self) -> usize {
        self.depth
    }
}
impl Drop for ModalDepthGuard {
    fn drop(&mut self) {
        MODAL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightAction {
    Recheck,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerFailureChoice {
    Retry,
    Fallback,
    Abort,
}

enum ModalHost {
    Overlay {
        parent_overlay: gtk::Overlay,
        scrim: gtk::EventBox,
        card: gtk::EventBox,
        key_source: gtk::Window,
    },
    Standalone(gtk::Window),
}

/// Pure frameless SEB modal dialog:
/// - When inside the running SEB shell (`parent` or registered `MAIN_OVERLAY`), mounts directly as an
///   in-window overlay scrim + centered card so Wayland/KWin/GTK NEVER draws any external OS titlebar.
/// - During startup (before main shell exists), falls back to a frameless `Splashscreen` toplevel window.
struct SebModal {
    window: gtk::Widget,
    body: gtk::Box,
    btn_row: gtk::Box,
    host: ModalHost,
    result: Rc<Cell<gtk::ResponseType>>,
    cancel_resp: gtk::ResponseType,
    buttons: Vec<(gtk::Button, gtk::ResponseType, bool)>,
}

impl SebModal {
    fn new(parent: Option<&gtk::Window>, title: &str, cancel_resp: gtk::ResponseType) -> Self {
        let card_box = gtk::EventBox::new();
        card_box.set_visible_window(true);
        card_box.style_context().add_class("seb-dialog");
        card_box.set_halign(gtk::Align::Center);
        card_box.set_valign(gtk::Align::Center);
        card_box.add_events(gdk::EventMask::KEY_PRESS_MASK | gdk::EventMask::BUTTON_PRESS_MASK);

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        title_row.style_context().add_class("seb-titlebar");
        title_row.set_size_request(-1, 26);
        let icon_img = assets::image_from_bytes(assets::ICON_SEB_PNG, 15, 15);
        icon_img.set_margin_end(6);
        title_row.pack_start(&icon_img, false, false, 0);
        let title_lbl = gtk::Label::new(Some(title));
        title_lbl.set_xalign(0.0);
        title_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_row.pack_start(&title_lbl, true, true, 0);
        outer.pack_start(&title_row, false, false, 0);

        let hairline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hairline.style_context().add_class("seb-hairline");
        hairline.set_size_request(-1, 1);
        outer.pack_start(&hairline, false, false, 0);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        body.set_margin_top(14);
        body.set_margin_bottom(12);
        body.set_margin_start(16);
        body.set_margin_end(16);
        outer.pack_start(&body, true, true, 0);

        let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        btn_row.set_halign(gtk::Align::End);
        btn_row.set_margin_top(2);
        btn_row.set_margin_bottom(14);
        btn_row.set_margin_start(16);
        btn_row.set_margin_end(16);
        outer.pack_end(&btn_row, false, false, 0);

        card_box.add(&outer);

        let overlay_opt = parent
            .and_then(|p| p.child())
            .and_then(|c| c.downcast::<gtk::Overlay>().ok())
            .or_else(|| MAIN_OVERLAY.with(|slot| slot.borrow().as_ref().and_then(|w| w.upgrade())));

        if let Some(parent_overlay) = overlay_opt {
            if let Some(key_source) = parent
                .cloned()
                .or_else(|| parent_overlay.toplevel().and_then(|w| w.downcast::<gtk::Window>().ok()))
            {
                let scrim = gtk::EventBox::new();
                scrim.set_visible_window(true);
                scrim.style_context().add_class("seb-modal-scrim");
                scrim.set_halign(gtk::Align::Fill);
                scrim.set_valign(gtk::Align::Fill);
                scrim.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
                scrim.connect_button_press_event(|_, _| glib::Propagation::Stop);
                scrim.add(&card_box);
                let widget: gtk::Widget = card_box.clone().upcast();
                return Self {
                    window: widget,
                    body,
                    btn_row,
                    host: ModalHost::Overlay {
                        parent_overlay,
                        scrim,
                        card: card_box,
                        key_source,
                    },
                    result: Rc::new(Cell::new(cancel_resp)),
                    cancel_resp,
                    buttons: Vec::new(),
                };
            }
        }

        let win = gtk::Window::new(gtk::WindowType::Toplevel);
        win.style_context().add_class("seb-dialog");
        win.set_title(title);
        make_window_frameless(&win);
        win.set_resizable(false);
        win.set_modal(true);
        win.set_skip_taskbar_hint(true);
        win.set_keep_above(true);
        win.set_position(gtk::WindowPosition::Center);
        win.add(&card_box);
        let widget: gtk::Widget = win.clone().upcast();
        Self {
            window: widget,
            body,
            btn_row,
            host: ModalHost::Standalone(win),
            result: Rc::new(Cell::new(cancel_resp)),
            cancel_resp,
            buttons: Vec::new(),
        }
    }

    fn set_default_size(&self, w: i32, h: i32) {
        self.window.set_size_request(w, h);
    }

    fn add_buttons(&mut self, defs: &[(&str, gtk::ResponseType, bool)]) -> Option<gtk::Button> {
        let mut focus_btn = None;
        for (label, resp, is_default) in defs {
            let btn = gtk::Button::with_label(label);
            btn.set_can_focus(true);
            btn.set_can_default(*is_default);
            self.btn_row.pack_start(&btn, false, false, 0);
            if *is_default {
                focus_btn = Some(btn.clone());
            }
            self.buttons.push((btn, *resp, *is_default));
        }
        focus_btn
    }

    fn run(self, initial_focus: Option<gtk::Widget>) -> gtk::ResponseType {
        let guard = ModalDepthGuard::enter();
        let my_depth = guard.depth();
        let loop_rc = Rc::new(glib::MainLoop::new(None, false));
        let res = Rc::clone(&self.result);
        let cancel = self.cancel_resp;

        for (btn, resp, _) in &self.buttons {
            let res_c = Rc::clone(&res);
            let lp_c = Rc::clone(&loop_rc);
            let r_val = *resp;
            btn.connect_clicked(move |_| {
                res_c.set(r_val);
                if lp_c.is_running() {
                    lp_c.quit();
                }
            });
            btn.connect_focus_in_event(|b, _| {
                b.style_context().add_class("seb-btn-focused");
                glib::Propagation::Proceed
            });
            btn.connect_focus_out_event(|b, _| {
                b.style_context().remove_class("seb-btn-focused");
                glib::Propagation::Proceed
            });
        }

        let btn_list: Vec<gtk::Button> = self.buttons.iter().map(|b| b.0.clone()).collect();
        let res_k = Rc::clone(&res);
        let lp_k = Rc::clone(&loop_rc);
        let default_btn_opt = self
            .buttons
            .iter()
            .find(|(_, _, def)| *def)
            .map(|b| b.0.clone());
        let def_btn_focus = default_btn_opt.clone();

        let handle_key = move |win: &gtk::Window, ev: &gdk::EventKey| -> glib::Propagation {
            // Only the topmost nested modal should process Escape / Return / Left / Right.
            if MODAL_DEPTH.with(|d| d.get()) != my_depth {
                return glib::Propagation::Proceed;
            }
            let kv = ev.keyval();
            if kv == gdk::keys::constants::Escape {
                res_k.set(cancel);
                if lp_k.is_running() {
                    lp_k.quit();
                }
                return glib::Propagation::Stop;
            }
            if (kv == gdk::keys::constants::Left || kv == gdk::keys::constants::Right)
                && !btn_list.is_empty()
            {
                let cur_idx = btn_list.iter().position(|b| b.has_focus());
                if let Some(idx) = cur_idx {
                    let next = if kv == gdk::keys::constants::Right {
                        (idx + 1) % btn_list.len()
                    } else {
                        (idx + btn_list.len() - 1) % btn_list.len()
                    };
                    btn_list[next].grab_focus();
                    return glib::Propagation::Stop;
                } else if win
                    .focused_widget()
                    .and_then(|w| w.downcast::<gtk::Entry>().ok())
                    .is_none()
                {
                    if let Some(first) = btn_list.first() {
                        first.grab_focus();
                        return glib::Propagation::Stop;
                    }
                }
            }
            if kv == gdk::keys::constants::Return || kv == gdk::keys::constants::KP_Enter {
                if let Some(focused) = win.focused_widget() {
                    if focused.is::<gtk::Entry>() || focused.is::<gtk::ListBoxRow>() {
                        return glib::Propagation::Proceed;
                    }
                }
                if let Some(focused_btn) = btn_list.iter().find(|b| b.has_focus()) {
                    focused_btn.clicked();
                    return glib::Propagation::Stop;
                } else if let Some(ref def_btn) = default_btn_opt {
                    def_btn.clicked();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        };

        match self.host {
            ModalHost::Overlay {
                parent_overlay,
                scrim,
                card,
                key_source,
            } => {
                let sig_id = key_source.connect_key_press_event(move |win, ev| handle_key(win, ev));
                card.connect_button_press_event(|_, _| glib::Propagation::Stop);
                parent_overlay.add_overlay(&scrim);
                scrim.show_all();
                if let Some(w) = initial_focus {
                    w.grab_focus();
                    if w.is::<gtk::Button>() {
                        w.style_context().add_class("seb-btn-focused");
                    }
                } else if let Some(ref def_btn) = def_btn_focus {
                    def_btn.grab_focus();
                    def_btn.style_context().add_class("seb-btn-focused");
                }
                loop_rc.run();
                key_source.disconnect(sig_id);
                parent_overlay.remove(&scrim);
                res.get()
            }
            ModalHost::Standalone(win) => {
                win.connect_key_press_event(move |w, ev| handle_key(w, ev));
                let res_del = Rc::clone(&res);
                let lp_del = Rc::clone(&loop_rc);
                win.connect_delete_event(move |_, _| {
                    res_del.set(cancel);
                    if lp_del.is_running() {
                        lp_del.quit();
                    }
                    glib::Propagation::Stop
                });
                win.show_all();
                if let Some(w) = initial_focus {
                    w.grab_focus();
                    if w.is::<gtk::Button>() {
                        w.style_context().add_class("seb-btn-focused");
                    }
                } else if let Some(ref def_btn) = def_btn_focus {
                    def_btn.grab_focus();
                    def_btn.style_context().add_class("seb-btn-focused");
                }
                win.present();
                loop_rc.run();
                win.hide();
                unsafe {
                    win.destroy();
                }
                res.get()
            }
        }
    }
}

/// 100% parity with C++ `RuntimeWindow` (`ui/runtime_window.cpp`):
/// Centered, frameless, always-on-top `720x320` startup/reconfiguration splash window
/// styled strictly with the SEB light palette.
pub struct RuntimeSplash {
    window: Option<gtk::Window>,
    overlay_scrim: Option<gtk::EventBox>,
    status_label: gtk::Label,
    progress: gtk::ProgressBar,
    alive: Rc<Cell<bool>>,
    shown_at: std::time::Instant,
}

impl RuntimeSplash {
    pub fn show(version: &str, status: &str) -> Self {
        Self::show_over(None, version, status)
    }

    pub fn show_over(parent: Option<&gtk::Window>, version: &str, status: &str) -> Self {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.style_context().add_class("seb-splash");
        outer.set_size_request(720, 320);

        let accent = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        accent.style_context().add_class("seb-splash-accent");
        accent.set_size_request(-1, 3);
        outer.pack_start(&accent, false, false, 0);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
        body.set_margin_top(28);
        body.set_margin_bottom(24);
        body.set_margin_start(36);
        body.set_margin_end(36);

        let top_row = gtk::Box::new(gtk::Orientation::Horizontal, 20);
        let left_col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let title = gtk::Label::new(None);
        title.set_markup("<span size='26000' weight='bold'>Safe Exam Browser</span>");
        title.set_xalign(0.0);
        let sub = gtk::Label::new(Some(
            "Preparing the secure exam session and browser environment.",
        ));
        sub.set_xalign(0.0);
        sub.style_context().add_class("seb-secondary-text");
        let ver = gtk::Label::new(Some(&format!("Version {version}")));
        ver.set_xalign(0.0);
        ver.style_context().add_class("seb-secondary-text");
        left_col.pack_start(&title, false, false, 0);
        left_col.pack_start(&sub, false, false, 0);
        left_col.pack_start(&ver, false, false, 4);
        top_row.pack_start(&left_col, true, true, 0);

        let logo = assets::image_from_bytes(assets::ICON_SEB_PNG, 104, 104);
        top_row.pack_end(&logo, false, false, 0);
        body.pack_start(&top_row, true, true, 0);

        let status_label = gtk::Label::new(Some(status));
        status_label.set_xalign(0.0);
        body.pack_start(&status_label, false, false, 0);

        let progress = gtk::ProgressBar::new();
        progress.set_pulse_step(0.08);
        progress.pulse();
        body.pack_start(&progress, false, false, 0);

        outer.pack_start(&body, true, true, 0);

        let alive = Rc::new(Cell::new(true));
        {
            let pb = progress.clone();
            let a = Rc::clone(&alive);
            glib::timeout_add_local(std::time::Duration::from_millis(60), move || {
                if !a.get() {
                    return glib::ControlFlow::Break;
                }
                pb.pulse();
                glib::ControlFlow::Continue
            });
        }

        // During reconfiguration (when the fullscreen shell is already up), mount directly
        // into MAIN_OVERLAY so it is guaranteed visible above the fullscreen locked shell on Wayland/X11!
        let main_overlay = if parent.is_some() {
            MAIN_OVERLAY.with(|cell| cell.borrow().as_ref().and_then(|w| w.upgrade()))
        } else {
            None
        };

        let (window, overlay_scrim) = if let Some(ov) = main_overlay {
            let scrim = gtk::EventBox::new();
            scrim.set_visible_window(true);
            scrim.style_context().add_class("seb-modal-scrim");
            scrim.set_halign(gtk::Align::Fill);
            scrim.set_valign(gtk::Align::Fill);

            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            card.style_context().add_class("seb-dialog");
            card.set_halign(gtk::Align::Center);
            card.set_valign(gtk::Align::Center);
            card.pack_start(&outer, true, true, 0);
            scrim.add(&card);

            ov.add_overlay(&scrim);
            ov.reorder_overlay(&scrim, -1);
            scrim.show_all();
            flush_gtk_events();
            std::thread::sleep(std::time::Duration::from_millis(16));
            flush_gtk_events();
            (None, Some(scrim))
        } else {
            let window = gtk::Window::new(gtk::WindowType::Toplevel);
            window.set_title("Safe Exam Browser");
            window.style_context().add_class("seb-splash");
            make_window_frameless(&window);
            window.set_resizable(false);
            window.set_skip_taskbar_hint(true);
            window.set_default_size(720, 320);
            window.set_size_request(720, 320);
            if let Some(p) = parent {
                window.set_transient_for(Some(p));
                window.set_position(gtk::WindowPosition::CenterOnParent);
            } else {
                window.set_position(gtk::WindowPosition::Center);
            }
            window.set_keep_above(true);
            window.add(&outer);
            window.show_all();
            flush_gtk_events();
            (Some(window), None)
        };

        Self {
            window,
            overlay_scrim,
            status_label,
            progress,
            alive,
            shown_at: std::time::Instant::now(),
        }
    }

    pub fn bring_to_front(&self) {
        if let Some(ref scrim) = self.overlay_scrim {
            MAIN_OVERLAY.with(|cell| {
                if let Some(ov) = cell.borrow().as_ref().and_then(|w| w.upgrade()) {
                    ov.reorder_overlay(scrim, -1);
                }
            });
            scrim.show_all();
            flush_gtk_events();
        }
    }

    pub fn finish_min_duration(&self, min_ms: u64) {
        let min_dur = std::time::Duration::from_millis(min_ms);
        while self.alive.get() && self.shown_at.elapsed() < min_dur {
            self.progress.pulse();
            flush_gtk_events();
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        self.close();
    }

    pub fn set_status(&self, status: &str) {
        self.status_label.set_text(status);
        self.progress.pulse();
        flush_gtk_events();
    }

    pub fn close(&self) {
        if !self.alive.replace(false) {
            return;
        }
        if let Some(ref scrim) = self.overlay_scrim {
            scrim.hide();
            MAIN_OVERLAY.with(|cell| {
                if let Some(ov) = cell.borrow().as_ref().and_then(|w| w.upgrade()) {
                    ov.remove(scrim);
                }
            });
        }
        if let Some(ref w) = self.window {
            w.hide();
            unsafe {
                w.destroy();
            }
        }
        flush_gtk_events();
    }
}

impl Drop for RuntimeSplash {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn flush_gtk_events() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

pub fn show_message(parent: Option<&gtk::Window>, title: &str, text: &str, is_error: bool) {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::Ok);
    dlg.set_default_size(440, -1);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon_name = if is_error {
        "dialog-error"
    } else {
        "dialog-information"
    };
    let icon = gtk::Image::from_icon_name(Some(icon_name), gtk::IconSize::Dialog);
    icon.set_valign(gtk::Align::Start);
    row.pack_start(&icon, false, false, 0);

    let text_col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let has_details = is_error && (text.contains('\n') || text.chars().count() > 140);
    let summary_text = if has_details {
        text.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(text)
            .to_string()
    } else {
        text.to_string()
    };

    let lbl = gtk::Label::new(Some(&summary_text));
    lbl.set_xalign(0.0);
    lbl.set_line_wrap(true);
    lbl.set_max_width_chars(56);
    text_col.pack_start(&lbl, false, false, 0);

    if has_details {
        let exp = gtk::Expander::new(Some("Show Details"));
        let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(110);
        scroll.set_max_content_height(220);
        let tv = gtk::TextView::new();
        tv.set_editable(false);
        tv.set_monospace(true);
        tv.set_wrap_mode(gtk::WrapMode::WordChar);
        if let Some(buf) = tv.buffer() {
            buf.set_text(text);
        }
        scroll.add(&tv);
        exp.add(&scroll);
        text_col.pack_start(&exp, true, true, 4);
    }

    row.pack_start(&text_col, true, true, 0);
    dlg.body.pack_start(&row, true, true, 0);

    if is_error {
        let copy_btn = gtk::Button::with_label("Copy");
        let full_text = text.to_string();
        copy_btn.connect_clicked(move |b| {
            let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
            cb.set_text(&full_text);
            b.set_label("Copied");
        });
        dlg.btn_row.pack_start(&copy_btn, false, false, 0);
    }

    let focus_btn = dlg.add_buttons(&[("OK", gtk::ResponseType::Ok, true)]);
    dlg.run(focus_btn.map(|b| b.upcast()));
}

pub fn show_confirm(parent: Option<&gtk::Window>, title: &str, text: &str) -> bool {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::No);
    dlg.set_default_size(400, -1);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon = gtk::Image::from_icon_name(Some("dialog-question"), gtk::IconSize::Dialog);
    icon.set_valign(gtk::Align::Start);
    row.pack_start(&icon, false, false, 0);

    let lbl = gtk::Label::new(Some(text));
    lbl.set_xalign(0.0);
    lbl.set_line_wrap(true);
    lbl.set_max_width_chars(56);
    row.pack_start(&lbl, true, true, 0);
    dlg.body.pack_start(&row, true, true, 0);

    let focus_btn = dlg.add_buttons(&[
        ("Yes", gtk::ResponseType::Yes, false),
        ("No", gtk::ResponseType::No, true),
    ]);
    dlg.run(focus_btn.map(|b| b.upcast())) == gtk::ResponseType::Yes
}

pub fn prompt_password(parent: Option<&gtk::Window>, title: &str, prompt: &str) -> Option<String> {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::Cancel);
    let ok_btn = dlg.add_buttons(&[
        ("Cancel", gtk::ResponseType::Cancel, false),
        ("OK", gtk::ResponseType::Ok, true),
    ]);

    let label = gtk::Label::new(Some(prompt));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    dlg.body.pack_start(&label, false, false, 0);

    let entry = gtk::Entry::new();
    entry.set_visibility(false);
    entry.set_width_chars(34);
    if let Some(btn) = ok_btn {
        entry.connect_activate(move |_| btn.clicked());
    }
    dlg.body.pack_start(&entry, false, false, 0);

    let resp = dlg.run(Some(entry.clone().upcast()));
    let value = entry.text().to_string();

    if resp == gtk::ResponseType::Ok {
        Some(value)
    } else {
        None
    }
}

pub fn prompt_text(
    parent: Option<&gtk::Window>,
    title: &str,
    prompt: &str,
    default_value: &str,
) -> Option<String> {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::Cancel);
    let ok_btn = dlg.add_buttons(&[
        ("Cancel", gtk::ResponseType::Cancel, false),
        ("OK", gtk::ResponseType::Ok, true),
    ]);

    let label = gtk::Label::new(Some(prompt));
    label.set_xalign(0.0);
    label.set_line_wrap(true);
    dlg.body.pack_start(&label, false, false, 0);

    let entry = gtk::Entry::new();
    entry.set_text(default_value);
    entry.set_width_chars(36);
    if let Some(btn) = ok_btn {
        entry.connect_activate(move |_| btn.clicked());
    }
    dlg.body.pack_start(&entry, false, false, 0);

    let resp = dlg.run(Some(entry.clone().upcast()));
    let value = entry.text().to_string();

    if resp == gtk::ResponseType::Ok {
        Some(value)
    } else {
        None
    }
}

pub fn prompt_credentials(
    parent: Option<&gtk::Window>,
    title: &str,
    realm: &str,
) -> Option<(String, String)> {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::Cancel);
    let ok_btn = dlg.add_buttons(&[
        ("Cancel", gtk::ResponseType::Cancel, false),
        ("OK", gtk::ResponseType::Ok, true),
    ]);

    let lbl = gtk::Label::new(Some(&format!("Authentication required for {realm}:")));
    lbl.set_xalign(0.0);
    lbl.set_line_wrap(true);
    dlg.body.pack_start(&lbl, false, false, 0);

    let user_entry = gtk::Entry::new();
    user_entry.set_placeholder_text(Some("Username"));
    dlg.body.pack_start(&user_entry, false, false, 0);

    let pass_entry = gtk::Entry::new();
    pass_entry.set_placeholder_text(Some("Password"));
    pass_entry.set_visibility(false);
    {
        let pe = pass_entry.clone();
        user_entry.connect_activate(move |_| pe.grab_focus());
    }
    if let Some(btn) = ok_btn {
        pass_entry.connect_activate(move |_| btn.clicked());
    }
    dlg.body.pack_start(&pass_entry, false, false, 0);

    let resp = dlg.run(Some(user_entry.clone().upcast()));
    let u = user_entry.text().to_string();
    let p = pass_entry.text().to_string();

    if resp == gtk::ResponseType::Ok {
        Some((u, p))
    } else {
        None
    }
}

fn extract_pid_from_label(entry: &str) -> Option<i32> {
    let start = entry.rfind("(PID ")? + 5;
    let rest = entry.get(start..)?;
    let end = rest.find(')')?;
    rest[..end].trim().parse::<i32>().ok().filter(|&p| p > 1)
}

fn terminate_pids(pids: &[i32]) {
    let self_pid = unsafe { libc::getpid() };
    let targets: Vec<i32> = pids
        .iter()
        .copied()
        .filter(|&p| p > 1 && p != self_pid)
        .collect();
    if targets.is_empty() {
        return;
    }
    for &pid in &targets {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(120));
    for &pid in &targets {
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        if alive {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// Two-phase anti-cheat pre-flight dialog (AGENTS.md):
/// Shown BEFORE kernel keyboard EVIOCGRAB so the user can switch windows and close
/// prohibited applications, then click Recheck or Quit.
pub fn show_prohibited_processes_preflight(detected: &[String]) -> PreflightAction {
    let mut dlg = SebModal::new(None, "Prohibited Applications Detected", gtk::ResponseType::Cancel);
    dlg.set_default_size(460, 340);
    let focus_btn = dlg.add_buttons(&[
        ("Quit", gtk::ResponseType::Cancel, false),
        ("Recheck", gtk::ResponseType::Accept, true),
    ]);

    let title = gtk::Label::new(Some(
        "The following prohibited applications must be closed before Safe Exam Browser can lock the system:",
    ));
    title.set_xalign(0.0);
    title.set_line_wrap(true);
    dlg.body.pack_start(&title, false, false, 0);

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_min_content_height(140);
    scroll.set_shadow_type(gtk::ShadowType::In);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let remaining = Rc::new(Cell::new(detected.len()));
    let all_pids: Vec<i32> = detected
        .iter()
        .filter_map(|s| extract_pid_from_label(s))
        .collect();

    for app in detected {
        let row_widget = gtk::ListBoxRow::new();
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        hbox.set_margin_top(2);
        hbox.set_margin_bottom(2);
        hbox.set_margin_start(6);
        hbox.set_margin_end(4);

        let lbl = gtk::Label::new(Some(&format!("• {app}")));
        lbl.set_xalign(0.0);
        lbl.set_hexpand(true);
        hbox.pack_start(&lbl, true, true, 0);

        let pid_opt = extract_pid_from_label(app);
        let close_btn = gtk::Button::with_label("×");
        close_btn.style_context().add_class("seb-proc-kill-btn");
        close_btn.set_can_focus(false);
        close_btn.set_tooltip_text(Some("Close process"));
        if pid_opt.is_none() {
            close_btn.set_sensitive(false);
        }
        let list_c = list.clone();
        let row_c = row_widget.clone();
        let rem_c = Rc::clone(&remaining);
        let recheck_opt = focus_btn.clone();
        close_btn.connect_clicked(move |_| {
            if let Some(pid) = pid_opt {
                terminate_pids(&[pid]);
            }
            list_c.remove(&row_c);
            let left = rem_c.get().saturating_sub(1);
            rem_c.set(left);
            if left == 0 {
                if let Some(ref btn) = recheck_opt {
                    btn.clicked();
                }
            }
        });
        hbox.pack_end(&close_btn, false, false, 0);

        row_widget.add(&hbox);
        list.add(&row_widget);
    }
    scroll.add(&list);
    dlg.body.pack_start(&scroll, true, true, 0);

    // Full-width single-line red "Kill All" button directly beneath the process table
    let kill_all_btn = gtk::Button::with_label("Kill All");
    kill_all_btn.style_context().add_class("seb-danger-btn");
    kill_all_btn.set_hexpand(true);
    kill_all_btn.set_can_focus(false);
    if all_pids.is_empty() {
        kill_all_btn.set_sensitive(false);
    }
    {
        let recheck_opt = focus_btn.clone();
        kill_all_btn.connect_clicked(move |_| {
            terminate_pids(&all_pids);
            if let Some(ref btn) = recheck_opt {
                btn.clicked();
            }
        });
    }
    dlg.body.pack_start(&kill_all_btn, false, false, 2);

    let hint = gtk::Label::new(Some(
        "Close these applications now (you can still switch windows), then click Recheck.",
    ));
    hint.set_xalign(0.0);
    hint.set_line_wrap(true);
    hint.style_context().add_class("seb-secondary-text");
    dlg.body.pack_start(&hint, false, false, 0);

    if dlg.run(focus_btn.map(|b| b.upcast())) == gtk::ResponseType::Accept {
        PreflightAction::Recheck
    } else {
        PreflightAction::Quit
    }
}

pub fn show_exam_selector(parent: Option<&gtk::Window>, exams: &[ExamInfo]) -> Option<ExamInfo> {
    if exams.is_empty() {
        return None;
    }
    if exams.len() == 1 {
        return Some(exams[0].clone());
    }

    let mut dlg = SebModal::new(parent, "Select Exam", gtk::ResponseType::Cancel);
    dlg.set_default_size(420, 280);
    let _ = dlg.add_buttons(&[
        ("Cancel", gtk::ResponseType::Cancel, false),
        ("Start Exam", gtk::ResponseType::Ok, true),
    ]);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    for exam in exams {
        let text = if exam.lms_name.is_empty() {
            exam.name.clone()
        } else {
            format!("{} ({})", exam.name, exam.lms_name)
        };
        let lbl = gtk::Label::new(Some(&text));
        lbl.set_xalign(0.0);
        lbl.set_margin_top(6);
        lbl.set_margin_bottom(6);
        lbl.set_margin_start(8);
        list.add(&lbl);
    }
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }
    dlg.body.pack_start(&list, true, true, 0);

    let list_c = list.clone();
    let resp = dlg.run(Some(list.upcast()));
    let idx = list_c.selected_row().map(|r| r.index()).unwrap_or(-1);

    if resp == gtk::ResponseType::Ok && idx >= 0 {
        exams.get(idx as usize).cloned()
    } else {
        None
    }
}

pub fn show_server_failure(
    parent: Option<&gtk::Window>,
    message: &str,
    allow_fallback: bool,
) -> ServerFailureChoice {
    let mut dlg = SebModal::new(parent, "SEB Server Unavailable", gtk::ResponseType::Cancel);

    let mut btns = vec![("Retry", gtk::ResponseType::Other(1), true)];
    if allow_fallback {
        btns.push(("Use Fallback", gtk::ResponseType::Other(2), false));
    }
    btns.push(("Abort", gtk::ResponseType::Cancel, false));
    let focus_btn = dlg.add_buttons(&btns);

    let lbl = gtk::Label::new(Some(message));
    lbl.set_line_wrap(true);
    lbl.set_xalign(0.0);
    dlg.body.pack_start(&lbl, false, false, 0);

    match dlg.run(focus_btn.map(|b| b.upcast())) {
        gtk::ResponseType::Other(1) => ServerFailureChoice::Retry,
        gtk::ResponseType::Other(2) => ServerFailureChoice::Fallback,
        _ => ServerFailureChoice::Abort,
    }
}

pub fn show_verificator_dialog(
    parent: Option<&gtk::Window>,
    app_ver: &str,
    compat_ver: &str,
    config_key: &str,
    bek: &str,
) {
    let mut dlg = SebModal::new(parent, "SEB Verificator", gtk::ResponseType::Close);
    dlg.set_default_size(560, 280);
    let focus_btn = dlg.add_buttons(&[("Close", gtk::ResponseType::Close, true)]);

    let hdr = gtk::Label::new(None);
    hdr.set_markup("<b>Safe Exam Browser Client Verification</b>");
    hdr.set_xalign(0.0);
    dlg.body.pack_start(&hdr, false, false, 0);

    let info = gtk::Label::new(Some(&format!(
        "Linux Version: {app_ver}  |  SEB Compatibility: {compat_ver}"
    )));
    info.set_xalign(0.0);
    info.style_context().add_class("seb-secondary-text");
    dlg.body.pack_start(&info, false, false, 0);

    let mk_field = |label: &str, value: &str| {
        let vb = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let l = gtk::Label::new(Some(label));
        l.set_xalign(0.0);
        let e = gtk::Entry::new();
        e.set_text(if value.is_empty() { "(none)" } else { value });
        e.set_editable(false);
        e.style_context().add_class("seb-url-entry");
        e.style_context().add_class("readonly");
        vb.pack_start(&l, false, false, 0);
        vb.pack_start(&e, false, false, 0);
        vb
    };

    dlg.body.pack_start(&mk_field("Configuration Key (CK):", config_key), false, false, 0);
    dlg.body.pack_start(&mk_field("Browser Exam Key (BEK):", bek), false, false, 0);

    dlg.run(focus_btn.map(|b| b.upcast()));
}

thread_local! {
    static ACTIVE_LOG_CLOSER: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

pub fn close_live_log_viewer() {
    let closer = ACTIVE_LOG_CLOSER.with(|cell| cell.borrow_mut().take());
    if let Some(f) = closer {
        f();
    }
}

/// Live, non-modal Application Log Window (4.1):
/// - Mounted directly inside MAIN_OVERLAY (never opens an external OS toplevel window,
///   so the main SEB window NEVER loses focus and AntiCheat never warns!)
/// - Singleton guard: clicking again toggles/closes cleanly; cleaned up on reconfig/shutdown.
pub fn show_live_log_viewer(_parent: Option<&gtk::Window>, logs: Rc<RefCell<Vec<String>>>) {
    // Toggle off if already open so duplicate stacked log cards & 500ms timers never accumulate
    if ACTIVE_LOG_CLOSER.with(|cell| cell.borrow().is_some()) {
        close_live_log_viewer();
        return;
    }
    let Some(ov) = MAIN_OVERLAY.with(|cell| cell.borrow().as_ref().and_then(|w| w.upgrade())) else {
        return;
    };

    let card = gtk::EventBox::new();
    card.set_visible_window(true);
    card.style_context().add_class("seb-dialog");
    card.style_context().add_class("seb-window-frame");
    card.style_context().add_class("windowed");
    card.set_size_request(660, 420);
    card.set_halign(gtk::Align::Start);
    card.set_valign(gtk::Align::Start);
    card.set_margin_start(120);
    card.set_margin_top(80);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Draggable 26px SEB titlebar
    let title_eb = gtk::EventBox::new();
    title_eb.add_events(
        gdk::EventMask::BUTTON_PRESS_MASK
            | gdk::EventMask::BUTTON_RELEASE_MASK
            | gdk::EventMask::POINTER_MOTION_MASK,
    );
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    title_row.style_context().add_class("seb-titlebar");
    title_row.set_size_request(-1, 26);
    let icon = assets::image_from_bytes(assets::ICON_LOG_SVG, 15, 15);
    let title_lbl = gtk::Label::new(Some("Application Log"));
    title_lbl.set_xalign(0.0);
    let close_x = gtk::Button::new();
    close_x.style_context().add_class("seb-title-btn");
    close_x.set_can_focus(false);
    close_x.set_size_request(34, 26);
    close_x.set_image(Some(&assets::image_from_bytes(
        assets::close_icon_svg(),
        13,
        13,
    )));
    title_row.pack_start(&icon, false, false, 0);
    title_row.pack_start(&title_lbl, true, true, 0);
    title_row.pack_end(&close_x, false, false, 0);
    title_eb.add(&title_row);
    outer.pack_start(&title_eb, false, false, 0);

    let hairline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hairline.style_context().add_class("seb-hairline");
    hairline.set_size_request(-1, 1);
    outer.pack_start(&hairline, false, false, 0);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(10);
    vbox.set_margin_start(10);
    vbox.set_margin_end(10);

    let ctrl_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let auto_scroll = gtk::CheckButton::with_label("Auto-Scroll");
    auto_scroll.set_active(true);
    let copy_btn = gtk::Button::with_label("Copy All");
    let close_btn = gtk::Button::with_label("Close");
    ctrl_row.pack_start(&auto_scroll, false, false, 0);
    ctrl_row.pack_end(&close_btn, false, false, 0);
    ctrl_row.pack_end(&copy_btn, false, false, 0);
    vbox.pack_start(&ctrl_row, false, false, 0);

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.style_context().add_class("seb-log-scroll");
    let view = gtk::TextView::new();
    view.style_context().add_class("seb-log-view");
    view.set_editable(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.set_top_margin(6);
    view.set_bottom_margin(6);
    scroll.add(&view);
    vbox.pack_start(&scroll, true, true, 0);
    outer.pack_start(&vbox, true, true, 0);
    card.add(&outer);

    // Drag-by-titlebar logic inside overlay
    let drag_st = Rc::new(RefCell::new((false, 0.0f64, 0.0f64, 120i32, 80i32)));
    {
        let ds = Rc::clone(&drag_st);
        let c = card.clone();
        title_eb.connect_button_press_event(move |_, ev| {
            if ev.button() == 1 {
                let (rx, ry) = ev.root();
                *ds.borrow_mut() = (true, rx, ry, c.margin_start(), c.margin_top());
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ds = Rc::clone(&drag_st);
        let c = card.clone();
        let ov_c = ov.clone();
        title_eb.connect_motion_notify_event(move |_, ev| {
            let (active, srx, sry, ox, oy) = *ds.borrow();
            if active {
                let (rx, ry) = ev.root();
                let max_x = (ov_c.allocated_width() - 660).max(0);
                let max_y = (ov_c.allocated_height() - 420).max(0);
                c.set_margin_start((ox + (rx - srx) as i32).clamp(0, max_x));
                c.set_margin_top((oy + (ry - sry) as i32).clamp(0, max_y));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ds = Rc::clone(&drag_st);
        title_eb.connect_button_release_event(move |_, _| {
            ds.borrow_mut().0 = false;
            glib::Propagation::Proceed
        });
    }

    let buf = view.buffer().unwrap();
    if let Some(tt) = buf.tag_table() {
        for (name, color) in [
            ("err", "#b71c1c"),
            ("warn", "#b26a00"),
            ("dbg", "#696969"),
            ("info", "#000000"),
        ] {
            let tag = gtk::TextTag::new(Some(name));
            tag.set_foreground(Some(color));
            tt.add(&tag);
        }
    }

    let append_colored_line = |b: &gtk::TextBuffer, line: &str| {
        let start_off = b.char_count();
        let mut end = b.end_iter();
        let with_nl = format!("{line}\n");
        b.insert(&mut end, &with_nl);
        let tag_name = if line.contains("[ERROR]") || line.contains("[CRITICAL]") {
            "err"
        } else if line.contains("[WARN]") || line.contains("[WARNING]") {
            "warn"
        } else if line.contains("[DEBUG]") {
            "dbg"
        } else {
            "info"
        };
        if let Some(tag) = b.tag_table().and_then(|tt| tt.lookup(tag_name)) {
            let s = b.iter_at_offset(start_off);
            let e = b.end_iter();
            b.apply_tag(&tag, &s, &e);
        }
    };

    let initial_len = {
        let guard = logs.borrow();
        for line in guard.iter() {
            append_colored_line(&buf, line);
        }
        guard.len()
    };

    {
        let logs_c = Rc::clone(&logs);
        copy_btn.connect_clicked(move |b| {
            let cb = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
            cb.set_text(&logs_c.borrow().join("\n"));
            b.set_label("Copied");
        });
    }

    let open_flag = Rc::new(Cell::new(true));
    let do_close: Rc<dyn Fn()> = {
        let of = Rc::clone(&open_flag);
        let c = card.clone();
        let ov_c = ov.clone();
        Rc::new(move || {
            if of.replace(false) {
                ACTIVE_LOG_CLOSER.with(|cell| {
                    *cell.borrow_mut() = None;
                });
                c.hide();
                ov_c.remove(&c);
                unsafe {
                    c.destroy();
                }
            }
        })
    };
    ACTIVE_LOG_CLOSER.with(|cell| {
        *cell.borrow_mut() = Some(Rc::clone(&do_close));
    });
    {
        let dc = Rc::clone(&do_close);
        close_btn.connect_clicked(move |_| dc());
    }
    {
        let dc = Rc::clone(&do_close);
        close_x.connect_clicked(move |_| dc());
    }

    let rendered = Rc::new(Cell::new(initial_len));
    {
        let logs_t = Rc::clone(&logs);
        let rendered_t = Rc::clone(&rendered);
        let open_t = Rc::clone(&open_flag);
        let buf_t = buf.clone();
        let view_t = view.clone();
        let auto_t = auto_scroll.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            if !open_t.get() {
                return glib::ControlFlow::Break;
            }
            let guard = logs_t.borrow();
            let cur = rendered_t.get();
            if guard.len() > cur {
                for line in &guard[cur..] {
                    append_colored_line(&buf_t, line);
                }
                rendered_t.set(guard.len());
                if auto_t.is_active() {
                    let mut end = buf_t.end_iter();
                    view_t.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    ov.add_overlay(&card);
    card.show_all();
    if auto_scroll.is_active() {
        let mut end = buf.end_iter();
        view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn matches_mime_or_ext(path: &Path, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    for f in filters {
        let fl = f.trim().to_ascii_lowercase();
        if fl == "*" || fl == "*/*" {
            return true;
        }
        if let Some(dot_ext) = fl.strip_prefix('.') {
            if ext == dot_ext {
                return true;
            }
        } else if fl.contains('/') {
            // Simple MIME-to-extension matching for common upload types
            let ok = match fl.as_str() {
                "application/pdf" => ext == "pdf",
                "image/*" => matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"),
                "image/png" => ext == "png",
                "image/jpeg" => matches!(ext.as_str(), "jpg" | "jpeg"),
                "text/plain" => matches!(ext.as_str(), "txt" | "log" | "md" | "csv"),
                "text/*" => matches!(ext.as_str(), "txt" | "csv" | "md" | "html" | "json" | "xml"),
                "application/zip" => ext == "zip",
                _ => true,
            };
            if ok {
                return true;
            }
        }
    }
    false
}

/// Custom SEB File System Dialog (4.3) — allows browsing ALL folders cleanly in SEB's
/// native light theme (Up, Home, Downloads, Root `/`, editable path bar, folder & file list).
fn run_seb_file_system_dialog(
    parent: Option<&gtk::Window>,
    title: &str,
    start_dir: &Path,
    save_mode: bool,
    suggested_name: Option<&str>,
    select_multiple: bool,
    mime_types: &[String],
) -> Vec<PathBuf> {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::Cancel);
    dlg.set_default_size(700, 480);

    let accept_label = if save_mode { "Save" } else { "Open" };
    let accept_btn_opt = dlg.add_buttons(&[
        ("Cancel", gtk::ResponseType::Cancel, false),
        (accept_label, gtk::ResponseType::Accept, true),
    ]);

    let initial_dir = if start_dir.is_dir() {
        start_dir.to_path_buf()
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
    };
    let current_dir = Rc::new(RefCell::new(initial_dir));
    let show_all_files = Rc::new(Cell::new(mime_types.is_empty()));
    let entries_data: Rc<RefCell<Vec<(PathBuf, bool)>>> = Rc::new(RefCell::new(Vec::new()));

    // Top navigation bar: [↑] [Home] [Downloads] [/] [Path Entry]
    let nav_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let up_btn = gtk::Button::with_label("↑ Up");
    up_btn.set_tooltip_text(Some("Parent folder"));
    let home_btn = gtk::Button::with_label("Home");
    let dl_btn = gtk::Button::with_label("Downloads");
    let root_btn = gtk::Button::with_label("/");
    let path_entry = gtk::Entry::new();
    path_entry.set_text(&current_dir.borrow().to_string_lossy());
    nav_bar.pack_start(&up_btn, false, false, 0);
    nav_bar.pack_start(&home_btn, false, false, 0);
    nav_bar.pack_start(&dl_btn, false, false, 0);
    nav_bar.pack_start(&root_btn, false, false, 0);
    nav_bar.pack_start(&path_entry, true, true, 0);
    dlg.body.pack_start(&nav_bar, false, false, 4);

    // High-contrast file list container with column header (Name | Size)
    let list_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_frame.style_context().add_class("seb-file-scroll");

    let col_hdr = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    col_hdr.style_context().add_class("seb-file-header");
    let col_name = gtk::Label::new(Some("Name"));
    col_name.set_xalign(0.0);
    let col_size = gtk::Label::new(Some("Size"));
    col_size.set_xalign(1.0);
    col_hdr.pack_start(&col_name, true, true, 0);
    col_hdr.pack_end(&col_size, false, false, 4);
    list_frame.pack_start(&col_hdr, false, false, 0);

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_min_content_height(310);
    let list_box = gtk::ListBox::new();
    list_box.style_context().add_class("seb-file-list");
    list_box.set_activate_on_single_click(false);
    list_box.set_selection_mode(if select_multiple && !save_mode {
        gtk::SelectionMode::Multiple
    } else {
        gtk::SelectionMode::Single
    });
    scroll.add(&list_box);
    list_frame.pack_start(&scroll, true, true, 0);
    dlg.body.pack_start(&list_frame, true, true, 4);

    // Bottom filename row
    let bottom_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name_lbl = gtk::Label::new(Some(if save_mode { "File name:" } else { "Selected:" }));
    let name_entry = gtk::Entry::new();
    if let Some(sn) = suggested_name {
        name_entry.set_text(sn);
    }
    bottom_row.pack_start(&name_lbl, false, false, 0);
    bottom_row.pack_start(&name_entry, true, true, 0);
    let filter_cb = if !mime_types.is_empty() && !save_mode {
        let cb = gtk::CheckButton::with_label("All files");
        cb.set_active(false);
        bottom_row.pack_end(&cb, false, false, 0);
        Some(cb)
    } else {
        None
    };
    dlg.body.pack_start(&bottom_row, false, false, 4);

    let filters_vec = Rc::new(mime_types.to_vec());
    let populate: Rc<dyn Fn(&Path)> = {
        let lb = list_box.clone();
        let pe = path_entry.clone();
        let cd = Rc::clone(&current_dir);
        let ed = Rc::clone(&entries_data);
        let sa = Rc::clone(&show_all_files);
        let fv = Rc::clone(&filters_vec);
        Rc::new(move |dir: &Path| {
            let target = if dir.is_dir() {
                dir.to_path_buf()
            } else {
                return;
            };
            *cd.borrow_mut() = target.clone();
            pe.set_text(&target.to_string_lossy());
            // Clear backing vector BEFORE removing widgets so row_selected never sees stale indices
            ed.borrow_mut().clear();
            for child in lb.children() {
                lb.remove(&child);
            }

            let mut dirs: Vec<(String, PathBuf)> = Vec::new();
            let mut files: Vec<(String, PathBuf, u64)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&target) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    let name = ent.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    if p.is_dir() {
                        dirs.push((name, p));
                    } else if sa.get() || matches_mime_or_ext(&p, &fv) {
                        let sz = ent.metadata().map(|m| m.len()).unwrap_or(0);
                        files.push((name, p, sz));
                    }
                }
            }
            dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

            // Parent folder entry at top when not at /
            if let Some(parent_p) = target.parent() {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let ico = gtk::Image::from_icon_name(Some("go-up"), gtk::IconSize::Menu);
                let lbl = gtk::Label::new(Some(".. (Parent Folder)"));
                lbl.set_xalign(0.0);
                row.pack_start(&ico, false, false, 0);
                row.pack_start(&lbl, true, true, 0);
                ed.borrow_mut().push((parent_p.to_path_buf(), true));
                lb.add(&row);
            }

            for (name, p) in dirs {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let ico = gtk::Image::from_icon_name(Some("folder"), gtk::IconSize::Menu);
                let lbl = gtk::Label::new(Some(&format!("{name}/")));
                lbl.set_xalign(0.0);
                let tag = gtk::Label::new(Some("Folder"));
                tag.style_context().add_class("seb-secondary-text");
                row.pack_start(&ico, false, false, 0);
                row.pack_start(&lbl, true, true, 0);
                row.pack_end(&tag, false, false, 0);
                ed.borrow_mut().push((p, true));
                lb.add(&row);
            }
            for (name, p, sz) in files {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let ico = gtk::Image::from_icon_name(Some("text-x-generic"), gtk::IconSize::Menu);
                let lbl = gtk::Label::new(Some(&name));
                lbl.set_xalign(0.0);
                let sz_lbl = gtk::Label::new(Some(&format_file_size(sz)));
                sz_lbl.style_context().add_class("seb-secondary-text");
                row.pack_start(&ico, false, false, 0);
                row.pack_start(&lbl, true, true, 0);
                row.pack_end(&sz_lbl, false, false, 0);
                ed.borrow_mut().push((p, false));
                lb.add(&row);
            }
            lb.show_all();
        })
    };

    // Clone path BEFORE calling populate so RefCell borrow is dropped first!
    let initial_path = current_dir.borrow().clone();
    populate(&initial_path);

    {
        let pop = Rc::clone(&populate);
        let cd = Rc::clone(&current_dir);
        up_btn.connect_clicked(move |_| {
            let parent_dir = cd.borrow().parent().map(|p| p.to_path_buf());
            if let Some(p) = parent_dir {
                pop(&p);
            }
        });
    }
    {
        let pop = Rc::clone(&populate);
        home_btn.connect_clicked(move |_| {
            if let Some(h) = std::env::var_os("HOME") {
                pop(Path::new(&h));
            }
        });
    }
    {
        let pop = Rc::clone(&populate);
        dl_btn.connect_clicked(move |_| {
            if let Some(h) = std::env::var_os("HOME") {
                let dl = PathBuf::from(h).join("Downloads");
                if dl.is_dir() {
                    pop(&dl);
                }
            }
        });
    }
    {
        let pop = Rc::clone(&populate);
        root_btn.connect_clicked(move |_| {
            pop(Path::new("/"));
        });
    }
    {
        let pop = Rc::clone(&populate);
        path_entry.connect_activate(move |e| {
            let p = PathBuf::from(e.text().as_str().trim());
            if p.is_dir() {
                pop(&p);
            }
        });
    }
    if let Some(cb) = filter_cb {
        let pop = Rc::clone(&populate);
        let cd = Rc::clone(&current_dir);
        let sa = Rc::clone(&show_all_files);
        cb.connect_toggled(move |c| {
            sa.set(c.is_active());
            let dir = cd.borrow().clone();
            pop(&dir);
        });
    }
    {
        let ed = Rc::clone(&entries_data);
        let ne = name_entry.clone();
        list_box.connect_row_selected(move |_, row_opt| {
            if let Some(r) = row_opt {
                let idx = r.index();
                if idx >= 0 {
                    if let Ok(guard) = ed.try_borrow() {
                        if let Some((path, is_dir)) = guard.get(idx as usize) {
                            if !*is_dir {
                                if let Some(n) = path.file_name().and_then(|x| x.to_str()) {
                                    ne.set_text(n);
                                }
                            }
                        }
                    }
                }
            }
        });
    }
    {
        let pop = Rc::clone(&populate);
        let ed = Rc::clone(&entries_data);
        let ne = name_entry.clone();
        let acc = accept_btn_opt.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let item = ed
                .try_borrow()
                .ok()
                .and_then(|g| g.get(idx as usize).cloned());
            if let Some((p, is_dir)) = item {
                if is_dir {
                    pop(&p);
                } else {
                    if let Some(n) = p.file_name().and_then(|x| x.to_str()) {
                        ne.set_text(n);
                    }
                    if let Some(ref b) = acc {
                        b.clicked();
                    }
                }
            }
        });
    }
    {
        let acc = accept_btn_opt.clone();
        name_entry.connect_activate(move |_| {
            if let Some(ref b) = acc {
                b.clicked();
            }
        });
    }

    let resp = dlg.run(Some(name_entry.clone().upcast()));
    if resp != gtk::ResponseType::Accept {
        return Vec::new();
    }

    if save_mode {
        let fname = name_entry.text().trim().to_string();
        if fname.is_empty() {
            return Vec::new();
        }
        return vec![current_dir.borrow().join(fname)];
    }

    let mut out = Vec::new();
    for row in list_box.selected_rows() {
        let idx = row.index() as usize;
        if let Some((path, is_dir)) = entries_data.borrow().get(idx) {
            if !*is_dir {
                out.push(path.clone());
            }
        }
    }
    if out.is_empty() {
        let typed = name_entry.text().trim().to_string();
        if !typed.is_empty() {
            let candidate = current_dir.borrow().join(typed);
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out
}

/// Custom SEB upload file chooser (4.3: browse all folders cleanly in SEB theme).
pub fn show_upload_file_chooser(
    parent: Option<&gtk::Window>,
    base_dir: &Path,
    select_multiple: bool,
    _allow_custom_location: bool,
    mime_types: &[String],
) -> Vec<PathBuf> {
    let title = if select_multiple {
        "Select Files to Upload"
    } else {
        "Select File to Upload"
    };
    run_seb_file_system_dialog(
        parent,
        title,
        base_dir,
        false,
        None,
        select_multiple,
        mime_types,
    )
}

/// Custom SEB Save-As dialog for downloads (4.3: browse all folders cleanly in SEB theme).
pub fn show_save_download_dialog(
    parent: Option<&gtk::Window>,
    base_dir: &Path,
    suggested_name: &str,
) -> Option<PathBuf> {
    run_seb_file_system_dialog(
        parent,
        "Save Download",
        base_dir,
        true,
        Some(suggested_name),
        false,
        &[],
    )
    .into_iter()
    .next()
}
