use crate::applications::{ApplicationManager, ManagedExternalApp};
use crate::assets;
use crate::dialogs;
use gtk::prelude::*;
use seb_config::settings::{AppSettings, SessionMode, UserInterfaceMode};
use seb_platform::{PlatformMonitor, PlatformSnapshot};
use seb_proctoring::client::ProctoringState;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub const TASKBAR_HEIGHT: i32 = 40;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum TaskbarAction {
    FocusWindow(u64),
    CloseWindow(u64),
    ShowAbout,
    ShowLog,
    ShowVerificator,
    RaiseHand(String),
    LowerHand,
    ToggleOnScreenKeyboard,
    RequestQuit,
}

pub struct SebTaskbar {
    pub root: gtk::Box,
    browser_active_bar: gtk::Box,
    browser_btn: gtk::Button,
    popup_layer: gtk::Overlay,
    popup_body: gtk::Box,
    popup_anchor: RefCell<Option<gtk::Widget>>,
    popup_key: RefCell<String>,
    window_items: RefCell<Vec<(u64, String, bool)>>,
    hover_hide_gen: Cell<u64>,
    ext_app_bars: RefCell<Vec<gtk::Box>>,
    proctoring_btn: gtk::Button,
    raise_hand_btn: Option<gtk::Button>,
    hand_raised: Cell<bool>,
    battery_btn: gtk::Button,
    network_btn: Option<gtk::Button>,
    audio_btn: Option<gtk::Button>,
    keyboard_btn: Option<gtk::Button>,
    clock_time_label: Option<gtk::Label>,
    clock_date_label: Option<gtk::Label>,
    last_snapshot: RefCell<PlatformSnapshot>,
    crit_thresh: f64,
    low_thresh: f64,
    force_raise_hand_msg: bool,
    on_action: Rc<dyn Fn(TaskbarAction)>,
}

impl SebTaskbar {
    pub fn new(
        settings: &AppSettings,
        platform: Rc<PlatformMonitor>,
        app_manager: Rc<RefCell<ApplicationManager>>,
        window_overlay: &gtk::Overlay,
        on_action: Rc<dyn Fn(TaskbarAction)>,
    ) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.style_context().add_class("seb-taskbar");
        root.set_size_request(-1, TASKBAR_HEIGHT);

        let tb_cfg = &settings.user_interface.taskbar;

        let mk_icon_btn = |bytes: &[u8], tip: &str, width: i32| -> gtk::Button {
            let b = gtk::Button::new();
            b.style_context().add_class("seb-chrome-btn");
            b.set_can_focus(false);
            b.set_size_request(width, TASKBAR_HEIGHT);
            b.set_tooltip_text(Some(tip));
            b.set_always_show_image(true);
            b.set_image(Some(&assets::image_from_bytes(bytes, 22, 22)));
            b
        };

        // =====================================================================
        // Left: Scrollable Application Strip (50x40px SEB + External Apps
        //       with 40x3px #1e90ff active bottom indicator bar!)
        // =====================================================================
        let app_scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
        app_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        app_scroll.set_size_request(-1, TASKBAR_HEIGHT);
        app_scroll.set_shadow_type(gtk::ShadowType::None);
        app_scroll.style_context().add_class("seb-app-scroll");

        let app_strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        app_scroll.add(&app_strip);
        root.pack_start(&app_scroll, true, true, 0);

        // Browser ApplicationControl (50x40 with #1e90ff bottom active bar)
        let browser_btn = gtk::Button::new();
        browser_btn.style_context().add_class("seb-chrome-btn");
        browser_btn.set_can_focus(false);
        browser_btn.set_size_request(50, TASKBAR_HEIGHT);
        browser_btn.set_tooltip_text(Some("Safe Exam Browser"));
        let browser_btn_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let browser_icon = assets::image_from_bytes(assets::ICON_SEB_PNG, 22, 22);
        browser_btn_box.pack_start(&browser_icon, true, true, 0);
        let browser_active_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        browser_active_bar.style_context().add_class("seb-taskbar-active-bar");
        browser_active_bar.set_size_request(40, 3);
        browser_active_bar.set_halign(gtk::Align::Center);
        browser_btn_box.pack_end(&browser_active_bar, false, false, 0);
        browser_btn.add(&browser_btn_box);
        app_strip.pack_start(&browser_btn, false, false, 0);

        // Shared rectangular taskbar popup overlay (pure flat box, zero GTK '>' / 'v' arrow tails!)
        // Keep `popup_layer` strictly ABOVE the 40px taskbar (`margin_bottom(TASKBAR_HEIGHT)`) so
        // its scrim never intercepts clicks or steals pointer hover from bottom-bar buttons!
        let popup_layer = gtk::Overlay::new();
        popup_layer.set_halign(gtk::Align::Fill);
        popup_layer.set_valign(gtk::Align::Fill);
        popup_layer.set_margin_bottom(TASKBAR_HEIGHT);
        let scrim = gtk::EventBox::new();
        scrim.set_visible_window(false);
        scrim.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
        popup_layer.add(&scrim);

        let popup_card = gtk::EventBox::new();
        popup_card.set_visible_window(false);
        popup_card.add_events(
            gdk::EventMask::BUTTON_PRESS_MASK
                | gdk::EventMask::ENTER_NOTIFY_MASK
                | gdk::EventMask::LEAVE_NOTIFY_MASK,
        );
        let popup_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        popup_body.style_context().add_class("seb-popup");
        popup_card.add(&popup_body);
        popup_layer.add_overlay(&popup_card);

        let popup_anchor: RefCell<Option<gtk::Widget>> = RefCell::new(None);
        let popup_key = RefCell::new(String::new());
        popup_layer.show_all();
        popup_layer.set_no_show_all(true);
        popup_layer.hide();
        window_overlay.add_overlay(&popup_layer);

        // External Application buttons (50x40 icon buttons with active bar + tooltip)
        let mut ext_bars = Vec::new();
        let app_count = app_manager.borrow().apps.len();
        let mut ext_app_buttons: Vec<(usize, gtk::Button)> = Vec::new();
        for idx in 0..app_count {
            let (show_in_shell, tip) = {
                let mgr = app_manager.borrow();
                let a = &mgr.apps[idx];
                (a.config.show_in_shell, a.tooltip())
            };
            if !show_in_shell {
                continue;
            }
            let b = gtk::Button::new();
            b.style_context().add_class("seb-chrome-btn");
            b.set_can_focus(false);
            b.set_size_request(50, TASKBAR_HEIGHT);
            b.set_tooltip_text(Some(&tip));
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vb.pack_start(
                &assets::image_from_bytes(assets::external_app_icon_svg(), 22, 22),
                true,
                true,
                0,
            );
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bar.style_context().add_class("seb-taskbar-inactive-bar");
            bar.set_size_request(40, 3);
            bar.set_halign(gtk::Align::Center);
            vb.pack_end(&bar, false, false, 0);
            b.add(&vb);
            ext_bars.push(bar);
            ext_app_buttons.push((idx, b.clone()));
            app_strip.pack_start(&b, false, false, 0);
        }

        // =====================================================================
        // Right: Exact C++ `taskbar_widget.cpp` ordering!
        // Notifications: About, Log, Verificator, Proctoring, RaiseHand
        // System: Battery, Network, Audio, Keyboard
        // Clock (stacked bold time + date in #696969)
        // Quit (hidden when !allow_termination)
        // =====================================================================
        if tb_cfg.show_application_info {
            let b = mk_icon_btn(assets::ICON_ABOUT_SVG, "Application information", 40);
            let cb = Rc::clone(&on_action);
            b.connect_clicked(move |_| cb(TaskbarAction::ShowAbout));
            root.pack_start(&b, false, false, 0);
        }

        if tb_cfg.show_application_log && settings.security.allow_application_log {
            let b = mk_icon_btn(assets::ICON_LOG_SVG, "Application log", 40);
            let cb = Rc::clone(&on_action);
            b.connect_clicked(move |_| cb(TaskbarAction::ShowLog));
            root.pack_start(&b, false, false, 0);
        }

        if tb_cfg.show_verificator {
            let b = mk_icon_btn(assets::ICON_VERIFICATOR_SVG, "SEB Verificator", 40);
            let cb = Rc::clone(&on_action);
            b.connect_clicked(move |_| cb(TaskbarAction::ShowVerificator));
            root.pack_start(&b, false, false, 0);
        }

        let proctoring_btn = mk_icon_btn(assets::ICON_PROCTORING_SVG, "Screen Proctoring (Window only): Inactive", 40);
        proctoring_btn.set_no_show_all(true);
        proctoring_btn.hide();
        root.pack_start(&proctoring_btn, false, false, 0);

        // Raise Hand — shown ONLY in SEB Server session when show_raise_hand == true!
        let raise_hand_btn =
            if settings.session_mode == SessionMode::Server && settings.server.show_raise_hand {
                let b = mk_icon_btn(assets::ICON_HAND_LOWERED_SVG, "Raise Hand", 40);
                root.pack_start(&b, false, false, 0);
                Some(b)
            } else {
                None
            };

        // System controls: Battery -> Network -> Audio -> Keyboard
        let battery_btn = mk_icon_btn(assets::ICON_BATTERY_SVG, "Battery", 40);
        battery_btn.set_no_show_all(true);
        battery_btn.hide();
        root.pack_start(&battery_btn, false, false, 0);

        let network_btn = if tb_cfg.show_network {
            let b = mk_icon_btn(assets::ICON_WIFI_0_SVG, "Network", 40);
            root.pack_start(&b, false, false, 0);
            Some(b)
        } else {
            None
        };

        let audio_btn = if tb_cfg.show_audio {
            let b = mk_icon_btn(assets::ICON_AUDIO_100_SVG, "Audio", 40);
            root.pack_start(&b, false, false, 0);
            Some(b)
        } else {
            None
        };

        let keyboard_btn = if tb_cfg.show_keyboard_layout {
            let b = gtk::Button::with_label("--");
            b.set_sensitive(false);
            b.style_context().add_class("seb-chrome-btn");
            b.set_can_focus(false);
            b.set_size_request(40, TASKBAR_HEIGHT);
            b.set_tooltip_text(Some("Keyboard layout unavailable"));
            root.pack_start(&b, false, false, 0);
            Some(b)
        } else {
            None
        };

        // Mobile / Touch UI Mode: On-Screen Keyboard (OSK) toggle button (4.6)
        if settings.user_interface.mode == UserInterfaceMode::Mobile {
            let osk_btn = mk_icon_btn(assets::keyboard_icon_svg(), "On-Screen Keyboard", 44);
            let cb = Rc::clone(&on_action);
            osk_btn.connect_clicked(move |_| cb(TaskbarAction::ToggleOnScreenKeyboard));
            root.pack_start(&osk_btn, false, false, 0);
        }

        // Stacked ClockWidget (bold time + short date in #696969, 15px horizontal margin)
        let (clock_time_label, clock_date_label) = if tb_cfg.show_clock {
            let clock_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            clock_box.set_margin_start(15);
            clock_box.set_margin_end(15);
            clock_box.set_homogeneous(true);
            clock_box.set_size_request(-1, TASKBAR_HEIGHT);
            let time_lbl = gtk::Label::new(Some("00:00"));
            time_lbl.set_yalign(1.0);
            time_lbl.style_context().add_class("seb-clock-time");
            let date_lbl = gtk::Label::new(Some("01/01/2026"));
            date_lbl.style_context().add_class("seb-clock-date");
            date_lbl.set_yalign(0.0);
            clock_box.pack_start(&time_lbl, false, false, 0);
            clock_box.pack_start(&date_lbl, false, false, 0);
            root.pack_start(&clock_box, false, false, 0);
            (Some(time_lbl), Some(date_lbl))
        } else {
            (None, None)
        };

        // Quit button — HIDDEN when !allow_termination (100% C++ parity!)
        if settings.security.allow_termination {
            let quit_btn = mk_icon_btn(assets::ICON_SHUTDOWN_SVG, "Quit Safe Exam Browser", 40);
            let act = Rc::clone(&on_action);
            quit_btn.connect_clicked(move |_| act(TaskbarAction::RequestQuit));
            root.pack_start(&quit_btn, false, false, 0);
        }

        let tb = Rc::new(Self {
            root,
            browser_active_bar,
            browser_btn: browser_btn.clone(),
            popup_layer: popup_layer.clone(),
            popup_body,
            popup_anchor,
            popup_key,
            window_items: RefCell::new(Vec::new()),
            hover_hide_gen: Cell::new(0),
            ext_app_bars: RefCell::new(ext_bars),
            proctoring_btn,
            raise_hand_btn: raise_hand_btn.clone(),
            hand_raised: Cell::new(false),
            battery_btn: battery_btn.clone(),
            network_btn: network_btn.clone(),
            audio_btn: audio_btn.clone(),
            keyboard_btn: keyboard_btn.clone(),
            clock_time_label,
            clock_date_label,
            last_snapshot: RefCell::new(platform.snapshot()),
            crit_thresh: settings.power_supply.charge_threshold_critical,
            low_thresh: settings.power_supply.charge_threshold_low,
            force_raise_hand_msg: settings.server.force_raise_hand_message,
            on_action,
        });

        // Position rectangular taskbar popup above its anchor button inside `popup_layer`
        {
            let card_w: gtk::Widget = popup_card.clone().upcast();
            let tb_pos = Rc::downgrade(&tb);
            popup_layer.connect_get_child_position(move |ov, child| {
                if child != &card_w {
                    return None;
                }
                let t = tb_pos.upgrade()?;
                let (_, nat) = child.preferred_size();
                let w = nat.width.max(60);
                let h = nat.height.max(32);
                let ov_w = ov.allocated_width().max(w + 16);
                let ov_h = ov.allocated_height().max(h + 16);
                if let Some(ref anc) = *t.popup_anchor.borrow() {
                    if let Some((ax, ay)) = anc.translate_coordinates(ov, 0, 0) {
                        let aw = anc.allocated_width();
                        let x = (ax + aw - w).clamp(4, (ov_w - w - 4).max(4));
                        let y = (ay - h).clamp(4, (ov_h - h).max(4));
                        return Some(gdk::Rectangle::new(x, y, w, h));
                    }
                }
                Some(gdk::Rectangle::new(4, (ov_h - h).max(4), w, h))
            });
        }
        {
            let t = Rc::clone(&tb);
            scrim.connect_button_press_event(move |_, _| {
                t.hide_popup();
                glib::Propagation::Stop
            });
        }

        // Wire external app buttons using shared arrow-free popup card
        for (idx, b) in ext_app_buttons {
            let mgr_c = Rc::clone(&app_manager);
            let t = Rc::clone(&tb);
            b.connect_clicked(move |btn| {
                let mut mgr = mgr_c.borrow_mut();
                if let Some(app) = mgr.apps.get_mut(idx) {
                    app.poll_windows();
                    let wins = app.windows();
                    if wins.is_empty() {
                        t.hide_popup();
                        app.start();
                    } else if wins.len() == 1 {
                        t.hide_popup();
                        ManagedExternalApp::activate_window(&wins[0].x11_id);
                    } else {
                        let key = format!("ext-{idx}");
                        let w_ref: gtk::Widget = btn.clone().upcast();
                        t.toggle_anchored_popup(&key, &w_ref, |box_c| {
                            let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
                            for w in wins {
                                let row = gtk::Button::with_label(&w.title);
                                row.style_context().add_class("seb-ac-win-row");
                                row.set_can_focus(false);
                                let wid = w.x11_id.clone();
                                let t2 = Rc::clone(&t);
                                row.connect_clicked(move |_| {
                                    t2.hide_popup();
                                    ManagedExternalApp::activate_window(&wid);
                                });
                                list.pack_start(&row, false, false, 0);
                            }
                            box_c.pack_start(&list, false, false, 0);
                        });
                    }
                }
            });
        }

        // Wire Browser ApplicationControl click (1 window -> focus immediately; >1 -> toggle popup) + hover!
        {
            let t = Rc::clone(&tb);
            browser_btn.connect_clicked(move |_| {
                let len = t.window_items.borrow().len();
                let first_id = t.window_items.borrow().first().map(|x| x.0);
                if len == 1 {
                    t.hide_popup();
                    if let Some(fid) = first_id {
                        (t.on_action)(TaskbarAction::FocusWindow(fid));
                    }
                } else if len > 1 {
                    if t.popup_layer.is_visible() && *t.popup_key.borrow() == "windows" {
                        t.hide_popup();
                    } else {
                        t.show_windows_popup();
                    }
                }
            });
            browser_btn.add_events(gdk::EventMask::ENTER_NOTIFY_MASK | gdk::EventMask::LEAVE_NOTIFY_MASK);
            let t_enter = Rc::clone(&tb);
            browser_btn.connect_enter_notify_event(move |_, _| {
                t_enter.hover_hide_gen.set(t_enter.hover_hide_gen.get().wrapping_add(1));
                if t_enter.window_items.borrow().len() > 1
                    && !(t_enter.popup_layer.is_visible()
                        && *t_enter.popup_key.borrow() == "windows")
                {
                    t_enter.show_windows_popup();
                }
                glib::Propagation::Proceed
            });
            let schedule_hover_hide = {
                let t_h = Rc::clone(&tb);
                let btn_w: gtk::Widget = browser_btn.clone().upcast();
                let card_w: gtk::Widget = popup_card.clone().upcast();
                Rc::new(move || {
                    let gen = t_h.hover_hide_gen.get().wrapping_add(1);
                    t_h.hover_hide_gen.set(gen);
                    let t2 = Rc::clone(&t_h);
                    let bw = btn_w.clone();
                    let cw = card_w.clone();
                    glib::timeout_add_local(Duration::from_millis(220), move || {
                        if t2.hover_hide_gen.get() != gen
                            || !t2.popup_layer.is_visible()
                            || *t2.popup_key.borrow() != "windows"
                        {
                            return glib::ControlFlow::Break;
                        }
                        if pointer_inside_widget(&bw, 0) || pointer_inside_widget(&cw, 8) {
                            return glib::ControlFlow::Continue;
                        }
                        t2.hide_popup();
                        glib::ControlFlow::Break
                    });
                })
            };

            let t_leave = Rc::clone(&schedule_hover_hide);
            browser_btn.connect_leave_notify_event(move |_, ev| {
                if ev.detail() == gdk::NotifyType::Inferior {
                    return glib::Propagation::Proceed;
                }
                t_leave();
                glib::Propagation::Proceed
            });
            let t_pop_enter = Rc::clone(&tb);
            popup_card.connect_enter_notify_event(move |_, _| {
                t_pop_enter.hover_hide_gen.set(t_pop_enter.hover_hide_gen.get().wrapping_add(1));
                glib::Propagation::Proceed
            });
            let t_pop_leave_tb = Rc::clone(&tb);
            let t_pop_leave = Rc::clone(&schedule_hover_hide);
            popup_card.connect_leave_notify_event(move |_, ev| {
                if ev.detail() == gdk::NotifyType::Inferior {
                    return glib::Propagation::Proceed;
                }
                if *t_pop_leave_tb.popup_key.borrow() == "windows" {
                    t_pop_leave();
                }
                glib::Propagation::Proceed
            });
        }

        // Wire Proctoring status popover
        {
            let t = Rc::clone(&tb);
            tb.proctoring_btn.connect_clicked(move |anchor| {
                let text = anchor
                    .tooltip_text()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Screen Proctoring: Active".to_string());
                let w_ref: gtk::Widget = anchor.clone().upcast();
                t.toggle_anchored_popup("proctoring", &w_ref, move |box_c| {
                    let lbl = gtk::Label::new(Some(&text));
                    lbl.set_margin_top(2);
                    lbl.set_margin_bottom(2);
                    lbl.set_margin_start(6);
                    lbl.set_margin_end(6);
                    box_c.pack_start(&lbl, false, false, 0);
                });
            });
        }

        // Wire RaiseHand inline popover (350px entry + Raise/Lower button)
        if let Some(ref rh_btn) = raise_hand_btn {
            let t = Rc::clone(&tb);
            rh_btn.connect_clicked(move |anchor| t.show_raise_hand_popover(anchor));
        }

        // Wire Battery details popover
        {
            let t = Rc::clone(&tb);
            battery_btn.connect_clicked(move |anchor| t.show_battery_popover(anchor));
        }

        // Wire Network Wi-Fi popover
        if let Some(ref net_btn) = network_btn {
            let t = Rc::clone(&tb);
            let plat = Rc::clone(&platform);
            net_btn.connect_clicked(move |anchor| t.show_network_popover(anchor, &plat));
        }

        // Wire Audio horizontal slider + mute icon + numeric % popover with live taskbar sync!
        if let Some(ref aud_btn) = audio_btn {
            let t = Rc::clone(&tb);
            let plat = Rc::clone(&platform);
            aud_btn.connect_clicked(move |anchor| t.show_audio_popover(anchor, &plat));
        }

        // Wire Keyboard layout switcher popover!
        if let Some(ref kb_btn) = keyboard_btn {
            let t = Rc::clone(&tb);
            let plat = Rc::clone(&platform);
            kb_btn.connect_clicked(move |anchor| t.show_keyboard_popover(anchor, &plat));
        }

        tb.apply_snapshot(&platform.snapshot());
        tb
    }

    pub fn remove_overlay(&self, window_overlay: &gtk::Overlay) {
        window_overlay.remove(&self.popup_layer);
        unsafe {
            self.popup_layer.destroy();
        }
    }

    pub fn is_popup_visible(&self) -> bool {
        self.popup_layer.is_visible()
    }

    pub fn hide_popup(&self) {
        self.popup_layer.hide();
        *self.popup_anchor.borrow_mut() = None;
        self.popup_key.borrow_mut().clear();
    }

    fn show_anchored_popup<F>(&self, key: &str, anchor: &gtk::Widget, build: F)
    where
        F: FnOnce(&gtk::Box),
    {
        for child in self.popup_body.children() {
            self.popup_body.remove(&child);
        }
        build(&self.popup_body);
        self.popup_body.show_all();
        *self.popup_anchor.borrow_mut() = Some(anchor.clone());
        *self.popup_key.borrow_mut() = key.to_string();
        self.popup_layer.show();
        self.popup_layer.queue_resize();
    }

    fn toggle_anchored_popup<F>(&self, key: &str, anchor: &gtk::Widget, build: F)
    where
        F: FnOnce(&gtk::Box),
    {
        if self.popup_layer.is_visible() && *self.popup_key.borrow() == key {
            self.hide_popup();
        } else {
            self.show_anchored_popup(key, anchor, build);
        }
    }

    fn show_windows_popup(self: &Rc<Self>) {
        let windows = self.window_items.borrow().clone();
        let w_ref: gtk::Widget = self.browser_btn.clone().upcast();
        self.show_anchored_popup("windows", &w_ref, |box_c| {
            let pop_scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
            pop_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            pop_scroll.set_max_content_height(400);
            pop_scroll.set_propagate_natural_height(true);
            let windows_list_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
            windows_list_box.set_size_request(260, -1);
            for (id, title, is_main) in &windows {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
                let btn = gtk::Button::new();
                btn.style_context().add_class("seb-ac-win-row");
                btn.set_can_focus(false);
                let hb = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                hb.pack_start(
                    &assets::image_from_bytes(assets::ICON_SEB_PNG, 20, 20),
                    false,
                    false,
                    0,
                );
                let lbl = gtk::Label::new(Some(title));
                lbl.set_xalign(0.0);
                lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                hb.pack_start(&lbl, true, true, 0);
                btn.add(&hb);
                let win_id = *id;
                let t = Rc::clone(self);
                btn.connect_clicked(move |_| {
                    t.hide_popup();
                    (t.on_action)(TaskbarAction::FocusWindow(win_id));
                });
                row_box.pack_start(&btn, true, true, 0);

                if !is_main {
                    let close = gtk::Button::new();
                    close.style_context().add_class("seb-chrome-btn");
                    close.set_can_focus(false);
                    close.set_size_request(28, 28);
                    close.set_image(Some(&assets::image_from_bytes(assets::close_icon_svg(), 12, 12)));
                    let t_c = Rc::clone(self);
                    close.connect_clicked(move |_| {
                        t_c.hide_popup();
                        (t_c.on_action)(TaskbarAction::CloseWindow(win_id));
                    });
                    row_box.pack_end(&close, false, false, 0);
                }
                windows_list_box.pack_start(&row_box, false, false, 0);
            }
            pop_scroll.add(&windows_list_box);
            box_c.pack_start(&pop_scroll, false, false, 0);
        });
    }

    pub fn update_window_list(&self, windows: &[(u64, String, bool)]) {
        *self.window_items.borrow_mut() = windows.to_vec();
        let bar_ctx = self.browser_active_bar.style_context();
        if windows.is_empty() {
            bar_ctx.remove_class("seb-taskbar-active-bar");
            bar_ctx.add_class("seb-taskbar-inactive-bar");
        } else {
            bar_ctx.remove_class("seb-taskbar-inactive-bar");
            bar_ctx.add_class("seb-taskbar-active-bar");
        }
    }

    pub fn update_external_app_indicators(&self, mgr: &ApplicationManager) {
        let bars = self.ext_app_bars.borrow();
        let mut bar_idx = 0;
        for app in &mgr.apps {
            if !app.config.show_in_shell {
                continue;
            }
            if let Some(bar) = bars.get(bar_idx) {
                let ctx = bar.style_context();
                if app.has_running_instances() {
                    ctx.remove_class("seb-taskbar-inactive-bar");
                    ctx.add_class("seb-taskbar-active-bar");
                } else {
                    ctx.remove_class("seb-taskbar-active-bar");
                    ctx.add_class("seb-taskbar-inactive-bar");
                }
            }
            bar_idx += 1;
        }
    }

    pub fn update_proctoring_status(&self, state: ProctoringState, text: &str, show_button: bool) {
        if state == ProctoringState::Inactive || !show_button {
            self.proctoring_btn.hide();
        } else {
            self.proctoring_btn.set_tooltip_text(Some(text));
            self.proctoring_btn
                .set_sensitive(state == ProctoringState::Active);
            self.proctoring_btn.show();
        }
    }

    pub fn set_hand_raised(&self, raised: bool) {
        self.hand_raised.set(raised);
        if let Some(ref btn) = self.raise_hand_btn {
            let svg = if raised {
                assets::ICON_HAND_RAISED_SVG
            } else {
                assets::ICON_HAND_LOWERED_SVG
            };
            btn.set_image(Some(&assets::image_from_bytes(svg, 22, 22)));
            btn.set_tooltip_text(Some(if raised { "Lower Hand" } else { "Raise Hand" }));
        }
    }

    pub fn refresh_clock(&self) {
        let now = chrono::Local::now();
        let tip = now.format("%A, %d %B %Y %H:%M:%S").to_string();
        if let Some(ref lbl) = self.clock_time_label {
            lbl.set_text(&now.format("%H:%M").to_string());
            lbl.set_tooltip_text(Some(&tip));
        }
        if let Some(ref lbl) = self.clock_date_label {
            lbl.set_text(&now.format("%d/%m/%Y").to_string());
            lbl.set_tooltip_text(Some(&tip));
        }
    }

    pub fn apply_snapshot(&self, snap: &PlatformSnapshot) {
        *self.last_snapshot.borrow_mut() = snap.clone();
        self.refresh_clock();

        // Battery (dynamic colored charge bar + lightning bolt + low/critical visual state)
        let bat = &snap.battery;
        if bat.available {
            self.battery_btn.set_image(Some(&assets::dynamic_battery_image(
                bat.percentage,
                bat.charging,
                self.crit_thresh,
                self.low_thresh,
                22,
            )));
            let ratio = bat.percentage as f64 / 100.0;
            let bctx = self.battery_btn.style_context();
            bctx.remove_class("seb-battery-low");
            bctx.remove_class("seb-battery-critical");
            let state_prefix = if !bat.charging && ratio <= self.crit_thresh {
                bctx.add_class("seb-battery-critical");
                "Critical battery: "
            } else if !bat.charging && ratio <= self.low_thresh {
                bctx.add_class("seb-battery-low");
                "Low battery: "
            } else if bat.charging {
                "Charging: "
            } else {
                "Battery remaining: "
            };
            let tip = format!("{state_prefix}{}% {}", bat.percentage, bat.time_remaining);
            self.battery_btn.set_tooltip_text(Some(tip.trim()));
            self.battery_btn.show();
        } else {
            self.battery_btn.hide();
        }

        // Network
        if let Some(ref btn) = self.network_btn {
            let net = &snap.network;
            btn.set_sensitive(net.available);
            let svg = if !net.available {
                assets::ICON_WIFI_0_SVG
            } else if net.kind == 1 {
                assets::ICON_NETWORK_WIRED_SVG
            } else if net.status == 2 {
                let sig = net
                    .networks
                    .iter()
                    .find(|n| n.active)
                    .map(|n| n.signal_percent)
                    .unwrap_or(0);
                if sig > 66 {
                    assets::ICON_WIFI_100_SVG
                } else if sig > 33 {
                    assets::ICON_WIFI_66_SVG
                } else if sig > 0 {
                    assets::ICON_WIFI_33_SVG
                } else {
                    assets::ICON_WIFI_0_SVG
                }
            } else {
                assets::ICON_WIFI_0_SVG
            };
            btn.set_image(Some(&assets::image_from_bytes(svg, 22, 22)));
            let tip = if !net.available {
                "Network unavailable".to_string()
            } else if net.kind == 1 {
                if net.status == 2 {
                    "Wired network connected".to_string()
                } else {
                    "Wired network disconnected".to_string()
                }
            } else if net.status == 2 {
                let ssid = net
                    .networks
                    .iter()
                    .find(|n| n.active)
                    .map(|n| n.ssid.as_str())
                    .unwrap_or(&net.active_connection);
                if ssid.is_empty() {
                    "Wireless network connected".to_string()
                } else {
                    format!("Connected to {ssid}")
                }
            } else if net.status == 1 {
                "Connecting to wireless network".to_string()
            } else {
                "Wireless network disconnected".to_string()
            };
            btn.set_tooltip_text(Some(&tip));
        }

        // Audio
        if let Some(ref btn) = self.audio_btn {
            let aud = &snap.audio;
            btn.set_sensitive(aud.available);
            btn.set_image(Some(&assets::image_from_bytes(audio_svg_for(aud), 22, 22)));
            let tip = if !aud.available {
                "Audio device not found".to_string()
            } else if aud.muted {
                "Audio muted".to_string()
            } else {
                format!("Audio volume: {}%", aud.volume_percent)
            };
            btn.set_tooltip_text(Some(&tip));
        }

        // Keyboard
        if let Some(ref btn) = self.keyboard_btn {
            let kb = &snap.keyboard;
            let short: String = if kb.current_short_name.is_empty() {
                "--".to_string()
            } else {
                kb.current_short_name.chars().take(3).collect()
            };
            btn.set_label(&short);
            btn.set_sensitive(!kb.current_short_name.is_empty());
            btn.set_tooltip_text(Some(if kb.current_long_name.is_empty() {
                "Keyboard layout unavailable"
            } else {
                &kb.current_long_name
            }));
        }
    }

    fn show_raise_hand_popover(self: &Rc<Self>, anchor: &gtk::Button) {
        let raised = self.hand_raised.get();
        let force_msg = self.force_raise_hand_msg;
        let t = Rc::clone(self);
        let w_ref: gtk::Widget = anchor.clone().upcast();
        self.toggle_anchored_popup("raise-hand", &w_ref, move |box_c| {
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 8);
            vb.set_size_request(350, -1);

            let hdr = gtk::Label::new(None);
            hdr.set_markup(if raised {
                "<b>Hand Raised</b>"
            } else {
                "<b>Raise Hand</b>"
            });
            hdr.set_xalign(0.0);
            vb.pack_start(&hdr, false, false, 0);

            let entry = gtk::Entry::new();
            entry.style_context().add_class("seb-url-entry");
            entry.set_placeholder_text(Some(if raised {
                "Lower your hand first to send a new message"
            } else if force_msg {
                "Type message for invigilator (required)..."
            } else {
                "Type optional message for invigilator..."
            }));
            entry.set_sensitive(!raised);
            vb.pack_start(&entry, false, false, 0);

            let action_btn = gtk::Button::with_label(if raised {
                "Lower Hand"
            } else {
                "Send & Raise Hand"
            });
            action_btn.style_context().add_class("seb-chrome-btn");
            action_btn.set_size_request(170, 30);
            action_btn.set_halign(gtk::Align::Center);

            let submit: Rc<dyn Fn()> = {
                let t_c = Rc::clone(&t);
                let e_c = entry.clone();
                Rc::new(move || {
                    if raised {
                        t_c.hide_popup();
                        (t_c.on_action)(TaskbarAction::LowerHand);
                    } else {
                        let text = e_c.text().trim().to_string();
                        if force_msg && text.is_empty() {
                            dialogs::show_message(
                                None,
                                "Message Required",
                                "Please enter a message before raising your hand.",
                                false,
                            );
                            return;
                        }
                        t_c.hide_popup();
                        (t_c.on_action)(TaskbarAction::RaiseHand(text));
                    }
                })
            };
            {
                let sub = Rc::clone(&submit);
                action_btn.connect_clicked(move |_| sub());
            }
            {
                let sub = Rc::clone(&submit);
                entry.connect_activate(move |_| sub());
            }
            if !raised {
                let e_focus = entry.clone();
                glib::idle_add_local_once(move || {
                    e_focus.grab_focus();
                });
            }
            vb.pack_start(&action_btn, false, false, 0);
            box_c.pack_start(&vb, false, false, 0);
        });
    }

    pub fn open_network_popup(self: &Rc<Self>, platform: &Rc<PlatformMonitor>) -> bool {
        if let Some(ref btn) = self.network_btn {
            self.show_network_popover(btn, platform);
            true
        } else {
            false
        }
    }

    pub fn open_audio_popup(self: &Rc<Self>, platform: &Rc<PlatformMonitor>) -> bool {
        if let Some(ref btn) = self.audio_btn {
            self.show_audio_popover(btn, platform);
            true
        } else {
            false
        }
    }

    pub fn open_battery_popup(self: &Rc<Self>) -> bool {
        if self.battery_btn.is_visible() {
            self.show_battery_popover(&self.battery_btn);
            true
        } else {
            false
        }
    }

    pub fn open_keyboard_popup(self: &Rc<Self>, platform: &Rc<PlatformMonitor>) -> bool {
        if let Some(ref btn) = self.keyboard_btn {
            self.show_keyboard_popover(btn, platform);
            true
        } else {
            false
        }
    }

    fn show_battery_popover(self: &Rc<Self>, anchor: &gtk::Button) {
        let bat = self.last_snapshot.borrow().battery.clone();
        if !bat.available {
            return;
        }
        let ratio = bat.percentage as f64 / 100.0;
        let prefix = if !bat.charging && ratio <= self.crit_thresh {
            "Critical battery — connect charger: "
        } else if !bat.charging && ratio <= self.low_thresh {
            "Low battery: "
        } else if bat.charging {
            "Charging: "
        } else {
            "Battery remaining: "
        };
        let text = if bat.time_remaining.is_empty() {
            format!("{prefix}{}%", bat.percentage)
        } else {
            format!("{prefix}{}% ({})", bat.percentage, bat.time_remaining)
        };
        let w_ref: gtk::Widget = anchor.clone().upcast();
        self.toggle_anchored_popup("battery", &w_ref, move |box_c| {
            let lbl = gtk::Label::new(Some(&text));
            lbl.set_margin_top(2);
            lbl.set_margin_bottom(2);
            lbl.set_margin_start(6);
            lbl.set_margin_end(6);
            box_c.pack_start(&lbl, false, false, 0);
        });
    }

    fn show_audio_popover(self: &Rc<Self>, anchor: &gtk::Button, platform: &Rc<PlatformMonitor>) {
        let st = self.last_snapshot.borrow().audio.clone();
        let t = Rc::clone(self);
        let plat = Rc::clone(platform);
        let w_ref: gtk::Widget = anchor.clone().upcast();
        self.toggle_anchored_popup("audio", &w_ref, move |box_c| {
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 6);

            let title = gtk::Label::new(Some(if st.available {
                "Default audio output"
            } else {
                "Audio device not found"
            }));
            vb.pack_start(&title, false, false, 0);

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let mute_btn = gtk::Button::new();
            mute_btn.style_context().add_class("seb-chrome-btn");
            mute_btn.set_can_focus(false);
            mute_btn.set_size_request(40, 36);
            mute_btn.set_image(Some(&assets::image_from_bytes(audio_svg_for(&st), 20, 20)));

            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 2.0);
            scale.set_value(st.volume_percent as f64);
            scale.set_draw_value(false);
            scale.set_size_request(250, -1);

            let pct_lbl = gtk::Label::new(Some(&st.volume_percent.to_string()));
            pct_lbl.set_size_request(40, -1);

            let muted_cell = Rc::new(Cell::new(st.muted));
            let vol_cell = Rc::new(Cell::new(st.volume_percent));

            {
                let lbl_c = pct_lbl.clone();
                let mb_c = mute_btn.clone();
                let t2 = Rc::clone(&t);
                let plat2 = Rc::clone(&plat);
                let m_c = Rc::clone(&muted_cell);
                let v_c = Rc::clone(&vol_cell);
                scale.connect_value_changed(move |s| {
                    let pct = s.value().round() as i32;
                    v_c.set(pct);
                    lbl_c.set_text(&pct.to_string());
                    seb_platform::set_audio_volume(pct);
                    plat2.update_audio_cached(None, Some(pct));
                    let cur = seb_platform::AudioState {
                        available: true,
                        muted: m_c.get(),
                        volume_percent: pct,
                    };
                    mb_c.set_image(Some(&assets::image_from_bytes(audio_svg_for(&cur), 20, 20)));
                    t2.apply_snapshot(&plat2.snapshot());
                });
            }
            {
                let t2 = Rc::clone(&t);
                let plat2 = Rc::clone(&plat);
                let m_c = Rc::clone(&muted_cell);
                let v_c = Rc::clone(&vol_cell);
                mute_btn.connect_clicked(move |b| {
                    let next = !m_c.get();
                    m_c.set(next);
                    seb_platform::set_audio_muted(next);
                    plat2.update_audio_cached(Some(next), None);
                    let cur = seb_platform::AudioState {
                        available: true,
                        muted: next,
                        volume_percent: v_c.get(),
                    };
                    b.set_image(Some(&assets::image_from_bytes(audio_svg_for(&cur), 20, 20)));
                    t2.apply_snapshot(&plat2.snapshot());
                });
            }

            row.pack_start(&mute_btn, false, false, 0);
            row.pack_start(&scale, true, true, 0);
            row.pack_start(&pct_lbl, false, false, 0);
            vb.pack_start(&row, false, false, 0);
            box_c.pack_start(&vb, false, false, 0);
        });
    }

    fn show_keyboard_popover(self: &Rc<Self>, anchor: &gtk::Button, platform: &Rc<PlatformMonitor>) {
        let kb = self.last_snapshot.borrow().keyboard.clone();
        let layouts = if kb.available_layouts.is_empty() {
            vec![if kb.current_short_name.is_empty() {
                "us".to_string()
            } else {
                kb.current_short_name.clone()
            }]
        } else {
            kb.available_layouts.clone()
        };
        let t = Rc::clone(self);
        let plat = Rc::clone(platform);
        let w_ref: gtk::Widget = anchor.clone().upcast();
        self.toggle_anchored_popup("keyboard", &w_ref, move |box_c| {
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 2);
            vb.set_size_request(180, -1);

            for (idx, code) in layouts.iter().enumerate() {
                let active = code.eq_ignore_ascii_case(&kb.current_short_name);
                let label = format!(
                    "{}{}",
                    if active { "•  " } else { "   " },
                    code.to_ascii_uppercase()
                );
                let btn = gtk::Button::with_label(&label);
                btn.style_context().add_class("seb-ac-win-row");
                btn.set_can_focus(false);
                btn.set_size_request(-1, 36);
                let code_c = code.clone();
                let plat2 = Rc::clone(&plat);
                let t2 = Rc::clone(&t);
                btn.connect_clicked(move |_| {
                    t2.hide_popup();
                    if seb_platform::switch_keyboard_layout(&code_c, idx) {
                        plat2.update_keyboard_cached(&code_c);
                        t2.apply_snapshot(&plat2.snapshot());
                    }
                });
                vb.pack_start(&btn, false, false, 0);
            }
            box_c.pack_start(&vb, false, false, 0);
        });
    }

    fn connect_wifi_async(
        self: &Rc<Self>,
        platform: &Rc<PlatformMonitor>,
        ssid: String,
        password: Option<String>,
        was_saved: bool,
    ) {
        let parent_win = self
            .root
            .toplevel()
            .and_then(|w| w.downcast::<gtk::Window>().ok());
        if let Some(ref nb) = self.network_btn {
            nb.set_tooltip_text(Some(&format!("Connecting to {ssid}...")));
        }
        let prompt_on_fail = was_saved && password.is_none();
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let ssid_bg = ssid.clone();
        thread::spawn(move || {
            let res = seb_platform::connect_wifi(&ssid_bg, password.as_deref());
            let _ = tx.send(res);
        });

        let t = Rc::clone(self);
        let plat = Rc::clone(platform);
        glib::timeout_add_local(Duration::from_millis(80), move || {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    let snap = plat.refresh_network_now();
                    t.apply_snapshot(&snap);
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    let snap = plat.refresh_network_now();
                    t.apply_snapshot(&snap);
                    // If a previously saved profile failed auth, prompt for a fresh password once
                    if prompt_on_fail {
                        if let Some(new_pw) = dialogs::prompt_password(
                            parent_win.as_ref(),
                            "Connect to Wi-Fi",
                            &format!("Saved credentials failed for {ssid}.\nEnter Wi-Fi password:"),
                        ) {
                            if !new_pw.is_empty() {
                                t.connect_wifi_async(&plat, ssid.clone(), Some(new_pw), false);
                                return glib::ControlFlow::Break;
                            }
                        }
                    }
                    dialogs::show_message(
                        parent_win.as_ref(),
                        "Wi-Fi Connection Failed",
                        &format!("Could not connect to \"{ssid}\":\n{err}"),
                        true,
                    );
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
            }
        });
    }

    fn show_network_popover(
        self: &Rc<Self>,
        anchor: &gtk::Button,
        platform: &Rc<PlatformMonitor>,
    ) {
        thread::spawn(|| {
            seb_platform::request_wifi_scan();
        });
        let st = platform.refresh_network_now().network;
        let t = Rc::clone(self);
        let plat = Rc::clone(platform);
        let w_ref: gtk::Widget = anchor.clone().upcast();
        self.toggle_anchored_popup("network", &w_ref, move |box_c| {
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 4);
            vb.set_size_request(260, -1);

            let hdr_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let hdr = gtk::Label::new(Some(if st.kind == 1 {
                "Wired Connection Active"
            } else if st.networks.is_empty() {
                "No Wi-Fi Networks Found"
            } else {
                "Wi-Fi Networks"
            }));
            hdr.set_xalign(0.0);
            hdr_row.pack_start(&hdr, true, true, 2);
            let refresh_btn = gtk::Button::with_label("↻");
            refresh_btn.style_context().add_class("seb-chrome-btn");
            refresh_btn.set_can_focus(false);
            refresh_btn.set_tooltip_text(Some("Refresh Wi-Fi list"));
            {
                let t_r = Rc::clone(&t);
                let p_r = Rc::clone(&plat);
                let anchor_c = anchor.clone();
                refresh_btn.connect_clicked(move |_| {
                    t_r.hide_popup();
                    t_r.show_network_popover(&anchor_c, &p_r);
                });
            }
            hdr_row.pack_end(&refresh_btn, false, false, 0);
            vb.pack_start(&hdr_row, false, false, 2);

            if !st.networks.is_empty() {
                let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
                scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
                scroll.set_max_content_height(320);
                scroll.set_propagate_natural_height(true);
                let list_box = gtk::Box::new(gtk::Orientation::Vertical, 2);

                for net in &st.networks {
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    let sig_svg = if net.signal_percent > 66 {
                        assets::ICON_WIFI_100_SVG
                    } else if net.signal_percent > 33 {
                        assets::ICON_WIFI_66_SVG
                    } else if net.signal_percent > 0 {
                        assets::ICON_WIFI_33_SVG
                    } else {
                        assets::ICON_WIFI_0_SVG
                    };
                    row.pack_start(&assets::image_from_bytes(sig_svg, 16, 16), false, false, 0);
                    let prefix = if net.active {
                        "✓ "
                    } else if net.saved {
                        "★ "
                    } else {
                        ""
                    };
                    let sec_tag = if net.secured { "" } else { " (Open)" };
                    let label = format!("{prefix}{}{sec_tag} ({}%)", net.ssid, net.signal_percent);
                    let lbl = gtk::Label::new(Some(&label));
                    lbl.set_xalign(0.0);
                    row.pack_start(&lbl, true, true, 0);

                    let btn = gtk::Button::new();
                    btn.add(&row);
                    btn.style_context().add_class("seb-ac-win-row");
                    btn.set_can_focus(false);
                    let ssid = net.ssid.clone();
                    let secured = net.secured;
                    let saved = net.saved;
                    let active = net.active;
                    let t2 = Rc::clone(&t);
                    let p2 = Rc::clone(&plat);
                    btn.connect_clicked(move |_| {
                        t2.hide_popup();
                        if active {
                            return;
                        }
                        let parent_win = t2
                            .root
                            .toplevel()
                            .and_then(|w| w.downcast::<gtk::Window>().ok());
                        if !secured || saved {
                            t2.connect_wifi_async(&p2, ssid.clone(), None, saved);
                        } else if let Some(pw) = dialogs::prompt_password(
                            parent_win.as_ref(),
                            "Connect to Wi-Fi",
                            &format!("Enter password for \"{ssid}\":"),
                        ) {
                            let pw_opt = (!pw.is_empty()).then_some(pw);
                            t2.connect_wifi_async(&p2, ssid.clone(), pw_opt, false);
                        }
                    });
                    list_box.pack_start(&btn, false, false, 0);
                }
                scroll.add(&list_box);
                vb.pack_start(&scroll, false, false, 0);
            }
            box_c.pack_start(&vb, false, false, 0);
        });
    }
}

fn pointer_inside_widget(w: &gtk::Widget, pad_bottom: i32) -> bool {
    let Some(toplevel) = w.toplevel() else {
        return false;
    };
    let Some(gdk_win) = toplevel.window() else {
        return false;
    };
    let Some(seat) = gdk_win.display().default_seat() else {
        return false;
    };
    let Some(ptr) = seat.pointer() else {
        return false;
    };
    let (_, px, py, _) = gdk_win.device_position(&ptr);
    let Some((wx, wy)) = w.translate_coordinates(&toplevel, 0, 0) else {
        return false;
    };
    let alloc = w.allocation();
    px >= wx && px <= wx + alloc.width() && py >= wy && py <= wy + alloc.height() + pad_bottom
}

pub fn audio_svg_for(st: &seb_platform::AudioState) -> &'static [u8] {
    if !st.available {
        assets::ICON_AUDIO_NONE_SVG
    } else if st.muted {
        assets::ICON_AUDIO_MUTED_SVG
    } else if st.volume_percent > 66 {
        assets::ICON_AUDIO_100_SVG
    } else if st.volume_percent > 33 {
        assets::ICON_AUDIO_66_SVG
    } else {
        assets::ICON_AUDIO_33_SVG
    }
}
