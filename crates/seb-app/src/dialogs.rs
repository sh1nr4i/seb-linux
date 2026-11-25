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
                win.close();
                res.get()
            }
        }
    }
}

/// 100% parity with C++ `RuntimeWindow` (`ui/runtime_window.cpp`):
/// Centered, frameless, always-on-top `720x320` startup/reconfiguration splash window
/// styled strictly with the SEB light palette.
pub struct RuntimeSplash {
    window: gtk::Window,
    status_label: gtk::Label,
    progress: gtk::ProgressBar,
}

impl RuntimeSplash {
    pub fn show(version: &str, status: &str) -> Self {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Safe Exam Browser");
        window.style_context().add_class("seb-splash");
        make_window_frameless(&window);
        window.set_resizable(false);
        window.set_skip_taskbar_hint(true);
        window.set_default_size(720, 320);
        window.set_size_request(720, 320);
        window.set_position(gtk::WindowPosition::Center);
        window.set_keep_above(true);

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
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
        progress.pulse();
        body.pack_start(&progress, false, false, 0);

        outer.pack_start(&body, true, true, 0);
        window.add(&outer);
        window.show_all();
        flush_gtk_events();

        Self {
            window,
            status_label,
            progress,
        }
    }

    pub fn set_status(&self, status: &str) {
        self.status_label.set_text(status);
        self.progress.pulse();
        flush_gtk_events();
    }

    pub fn close(&self) {
        self.window.hide();
        self.window.close();
        flush_gtk_events();
    }
}

pub fn flush_gtk_events() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

pub fn show_message(parent: Option<&gtk::Window>, title: &str, text: &str, is_error: bool) {
    let mut dlg = SebModal::new(parent, title, gtk::ResponseType::Ok);
    dlg.set_default_size(400, -1);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon_name = if is_error {
        "dialog-error"
    } else {
        "dialog-information"
    };
    let icon = gtk::Image::from_icon_name(Some(icon_name), gtk::IconSize::Dialog);
    icon.set_valign(gtk::Align::Start);
    row.pack_start(&icon, false, false, 0);

    let lbl = gtk::Label::new(Some(text));
    lbl.set_xalign(0.0);
    lbl.set_line_wrap(true);
    lbl.set_max_width_chars(56);
    row.pack_start(&lbl, true, true, 0);
    dlg.body.pack_start(&row, true, true, 0);

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
    let _ = dlg.add_buttons(&[
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
    let _ = dlg.add_buttons(&[
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
    let _ = dlg.add_buttons(&[
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

/// Two-phase anti-cheat pre-flight dialog (AGENTS.md):
/// Shown BEFORE kernel keyboard EVIOCGRAB so the user can switch windows and close
/// prohibited applications, then click Recheck or Quit.
pub fn show_prohibited_processes_preflight(detected: &[String]) -> PreflightAction {
    let mut dlg = SebModal::new(None, "Prohibited Applications Detected", gtk::ResponseType::Cancel);
    dlg.set_default_size(460, 300);
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
    for app in detected {
        let row = gtk::Label::new(Some(&format!("  • {app}")));
        row.set_xalign(0.0);
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        list.add(&row);
    }
    scroll.add(&list);
    dlg.body.pack_start(&scroll, true, true, 0);

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

pub fn show_log_viewer(parent: Option<&gtk::Window>, logs: &[String]) {
    let mut dlg = SebModal::new(parent, "Application Log", gtk::ResponseType::Close);
    dlg.set_default_size(640, 440);
    let focus_btn = dlg.add_buttons(&[("Close", gtk::ResponseType::Close, true)]);

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_monospace(true);
    if let Some(buf) = view.buffer() {
        buf.set_text(&logs.join("\n"));
    }
    scroll.add(&view);
    dlg.body.pack_start(&scroll, true, true, 0);
    dlg.run(focus_btn.map(|b| b.upcast()));
}

/// Sandboxed file upload chooser matching C++ `browser_window.cpp:304-429`:
/// - Enforces `base_dir` jail with canonical path check when `!allow_custom_location` (anti-symlink escape).
/// - Applies `mime_types` filter.
/// - Always `keep_above(true)` so it stays above the fullscreen locked shell.
pub fn show_upload_file_chooser(
    parent: Option<&gtk::Window>,
    base_dir: &Path,
    select_multiple: bool,
    allow_custom_location: bool,
    mime_types: &[String],
) -> Vec<PathBuf> {
    let dlg = gtk::FileChooserDialog::with_buttons(
        Some(if select_multiple {
            "Select Files to Upload"
        } else {
            "Select File to Upload"
        }),
        parent,
        gtk::FileChooserAction::Open,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Open", gtk::ResponseType::Accept),
        ],
    );
    dlg.style_context().add_class("seb-dialog");
    make_window_frameless(&dlg);
    dlg.set_keep_above(true);
    dlg.set_select_multiple(select_multiple);
    let _ = dlg.set_current_folder(base_dir);

    if !mime_types.is_empty() {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Accepted Files"));
        for mt in mime_types {
            if mt.contains('/') {
                filter.add_mime_type(mt);
            } else if mt.starts_with('.') {
                filter.add_pattern(&format!("*{mt}"));
            }
        }
        dlg.add_filter(filter);
    }

    let canonical_base = std::fs::canonicalize(base_dir).unwrap_or_else(|_| base_dir.to_path_buf());
    if !allow_custom_location {
        let base_c = canonical_base.clone();
        dlg.connect_current_folder_changed(move |fc| {
            if let Some(cur) = fc.current_folder() {
                let canon_cur = std::fs::canonicalize(&cur).unwrap_or(cur);
                if !canon_cur.starts_with(&base_c) {
                    let _ = fc.set_current_folder(&base_c);
                }
            }
        });
    }

    let resp = dlg.run();
    let files = if resp == gtk::ResponseType::Accept {
        dlg.filenames()
    } else {
        Vec::new()
    };
    dlg.close();

    if allow_custom_location {
        files
    } else {
        // Canonical path containment check prevents symlink escape out of base_dir!
        files
            .into_iter()
            .filter(|f| {
                std::fs::canonicalize(f)
                    .map(|c| c.starts_with(&canonical_base))
                    .unwrap_or(false)
            })
            .collect()
    }
}

/// Save-As dialog for downloads when `allow_custom_down_upload_location == true`.
pub fn show_save_download_dialog(
    parent: Option<&gtk::Window>,
    base_dir: &Path,
    suggested_name: &str,
) -> Option<PathBuf> {
    let dlg = gtk::FileChooserDialog::with_buttons(
        Some("Save Download"),
        parent,
        gtk::FileChooserAction::Save,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Save", gtk::ResponseType::Accept),
        ],
    );
    dlg.style_context().add_class("seb-dialog");
    make_window_frameless(&dlg);
    dlg.set_keep_above(true);
    dlg.set_do_overwrite_confirmation(true);
    let _ = dlg.set_current_folder(base_dir);
    dlg.set_current_name(suggested_name);

    let resp = dlg.run();
    let path = if resp == gtk::ResponseType::Accept {
        dlg.filename()
    } else {
        None
    };
    dlg.close();
    path
}
