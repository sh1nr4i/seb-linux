use crate::assets;
use crate::browser_pane::BrowserPane;
use crate::taskbar::audio_svg_for;
use gtk::prelude::*;
use seb_config::settings::AppSettings;
use seb_platform::{PlatformMonitor, PlatformSnapshot};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const SIDEBAR_WIDTH: i32 = 368;
const CARD_WIDTH: i32 = 224;
const CARD_HEIGHT: i32 = 160;
const THUMB_WIDTH: i32 = 204;
const THUMB_HEIGHT: i32 = 114;
const MAX_COLUMNS: i32 = 4;

// =========================================================================
// Action Center Sidebar (Super+A -> Ctrl+Shift+A):
// - Transparent click-outside scrim for instant outside-click dismissal!
// - Dynamic icons & live 1s refresh while visible!
// - Application Log tile, clickable Audio/Keyboard tiles, accurate Battery text!
// =========================================================================

pub struct ActionCenterOverlay {
    pub root: gtk::Box,
    windows_box: gtk::Box,
    clock_label: Option<gtk::Label>,
    audio_tile: Option<(gtk::Image, gtk::Label)>,
    battery_tile: Option<(gtk::Image, gtk::Label)>,
    network_tile: Option<(gtk::Image, gtk::Label)>,
    keyboard_label: Option<gtk::Label>,
    crit_thresh: f64,
    low_thresh: f64,
}

impl ActionCenterOverlay {
    pub fn new(
        settings: &AppSettings,
        platform: Rc<PlatformMonitor>,
        on_about: Rc<dyn Fn()>,
        on_log: Rc<dyn Fn()>,
        on_quit: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        // Horizontal full-width overlay container: [368px Sidebar | Transparent click-outside Scrim]
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_halign(gtk::Align::Fill);
        root.set_valign(gtk::Align::Fill);
        root.set_no_show_all(true);
        root.hide();

        let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
        panel.style_context().add_class("seb-action-center");
        panel.set_size_request(SIDEBAR_WIDTH, -1);
        root.pack_start(&panel, false, false, 0);

        // Transparent click-outside scrim (dismisses Action Center on click outside!)
        let scrim = gtk::EventBox::new();
        scrim.set_visible_window(false);
        scrim.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
        {
            let r = root.clone();
            scrim.connect_button_press_event(move |_, _| {
                r.hide();
                glib::Propagation::Stop
            });
        }
        root.pack_start(&scrim, true, true, 0);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_top(10);
        inner.set_margin_bottom(10);
        inner.set_margin_start(10);
        inner.set_margin_end(10);
        panel.pack_start(&inner, true, true, 0);

        let header = gtk::Label::new(Some("Browser"));
        header.set_markup("<b>Browser</b>");
        inner.pack_start(&header, false, false, 4);

        let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let windows_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        scroll.add(&windows_box);
        inner.pack_start(&scroll, true, true, 0);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(6);
        grid.set_column_spacing(6);
        grid.set_column_homogeneous(true);
        inner.pack_start(&grid, false, false, 4);

        let ac = &settings.user_interface.action_center;
        let mut idx = 0i32;

        let mk_tile = |svg: &[u8], text: &str| -> (gtk::Button, gtk::Image, gtk::Label) {
            let btn = gtk::Button::new();
            btn.style_context().add_class("seb-ac-tile");
            btn.set_can_focus(false);
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 4);
            vb.set_valign(gtk::Align::Center);
            let img = assets::image_from_bytes(svg, 22, 22);
            vb.pack_start(&img, false, false, 0);
            let lbl = gtk::Label::new(Some(text));
            lbl.set_justify(gtk::Justification::Center);
            lbl.set_line_wrap(true);
            vb.pack_start(&lbl, false, false, 0);
            btn.add(&vb);
            (btn, img, lbl)
        };

        let mut add_tile = |btn: &gtk::Button| {
            grid.attach(btn, idx % MAX_COLUMNS, idx / MAX_COLUMNS, 1, 1);
            idx += 1;
        };

        if ac.show_application_info {
            let (btn, _, _) = mk_tile(assets::ICON_ABOUT_SVG, "Information\nabout SEB");
            let cb = Rc::clone(&on_about);
            let r = root.clone();
            btn.connect_clicked(move |_| {
                r.hide();
                cb();
            });
            add_tile(&btn);
        }

        if ac.show_application_log && settings.security.allow_application_log {
            let (btn, _, _) = mk_tile(assets::ICON_LOG_SVG, "Application\nLog");
            let cb = Rc::clone(&on_log);
            let r = root.clone();
            btn.connect_clicked(move |_| {
                r.hide();
                cb();
            });
            add_tile(&btn);
        }

        let keyboard_label = if ac.show_keyboard_layout {
            let (btn, _, lbl) = mk_tile(assets::keyboard_icon_svg(), "Keyboard");
            let plat = Rc::clone(&platform);
            let lbl_c = lbl.clone();
            btn.connect_clicked(move |_| {
                let kb = plat.snapshot().keyboard;
                if kb.available_layouts.len() > 1 {
                    let cur = kb
                        .available_layouts
                        .iter()
                        .position(|l| l.eq_ignore_ascii_case(&kb.current_short_name))
                        .unwrap_or(0);
                    let next = (cur + 1) % kb.available_layouts.len();
                    let next_code = &kb.available_layouts[next];
                    if seb_platform::switch_keyboard_layout(next_code, next) {
                        plat.update_keyboard_cached(next_code);
                        lbl_c.set_text(&next_code.to_ascii_uppercase());
                    }
                }
            });
            add_tile(&btn);
            Some(lbl)
        } else {
            None
        };

        let network_tile = if ac.show_network {
            let (btn, img, lbl) = mk_tile(assets::ICON_WIFI_0_SVG, "Network");
            add_tile(&btn);
            Some((img, lbl))
        } else {
            None
        };

        let audio_tile = if ac.show_audio {
            let (btn, img, lbl) = mk_tile(assets::ICON_AUDIO_100_SVG, "Audio");
            let plat = Rc::clone(&platform);
            let img_c = img.clone();
            let lbl_c = lbl.clone();
            btn.connect_clicked(move |_| {
                let st = plat.snapshot().audio;
                if st.available {
                    let next_muted = !st.muted;
                    seb_platform::set_audio_muted(next_muted);
                    plat.update_audio_cached(Some(next_muted), None);
                    let updated = plat.snapshot().audio;
                    if let Some(pb) = assets::pixbuf_from_bytes(audio_svg_for(&updated), 22, 22) {
                        img_c.set_from_pixbuf(Some(&pb));
                    }
                    lbl_c.set_text(if updated.muted {
                        "Muted"
                    } else {
                        "Audio"
                    });
                }
            });
            add_tile(&btn);
            Some((img, lbl))
        } else {
            None
        };

        let battery_tile = {
            let st = platform.snapshot().battery;
            if st.available {
                let (btn, img, lbl) =
                    mk_tile(assets::ICON_BATTERY_SVG, &format!("Battery\n({}%)", st.percentage));
                add_tile(&btn);
                Some((img, lbl))
            } else {
                None
            }
        };

        let clock_label = if ac.show_clock {
            let (btn, _, lbl) = mk_tile(assets::clock_icon_svg(), "00:00:00");
            add_tile(&btn);
            Some(lbl)
        } else {
            None
        };

        let (quit_btn, _, _) = mk_tile(assets::ICON_SHUTDOWN_SVG, "Terminate\nSession");
        quit_btn.set_sensitive(settings.security.allow_termination);
        {
            let cb = Rc::clone(&on_quit);
            let r = root.clone();
            quit_btn.connect_clicked(move |_| {
                r.hide();
                cb();
            });
        }
        add_tile(&quit_btn);

        Rc::new(Self {
            root,
            windows_box,
            clock_label,
            audio_tile,
            battery_tile,
            network_tile,
            keyboard_label,
            crit_thresh: settings.power_supply.charge_threshold_critical,
            low_thresh: settings.power_supply.charge_threshold_low,
        })
    }

    pub fn toggle(
        &self,
        panes: &[Rc<BrowserPane>],
        snap: &PlatformSnapshot,
        on_focus: Rc<dyn Fn(u64)>,
    ) {
        if self.root.is_visible() {
            self.root.hide();
        } else {
            self.refresh_windows(panes, on_focus);
            self.refresh_tiles(snap);
            self.root.set_no_show_all(false);
            self.root.show_all();
        }
    }

    pub fn hide(&self) {
        self.root.hide();
    }

    pub fn is_visible(&self) -> bool {
        self.root.is_visible()
    }

    pub fn refresh_windows(&self, panes: &[Rc<BrowserPane>], on_focus: Rc<dyn Fn(u64)>) {
        for c in self.windows_box.children() {
            self.windows_box.remove(&c);
        }
        for pane in panes {
            let btn = gtk::Button::new();
            btn.style_context().add_class("seb-ac-win-row");
            btn.set_can_focus(false);
            let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            hb.pack_start(
                &assets::image_from_bytes(assets::ICON_SEB_PNG, 20, 20),
                false,
                false,
                0,
            );
            let lbl = gtk::Label::new(Some(&pane.title()));
            lbl.set_xalign(0.0);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            hb.pack_start(&lbl, true, true, 0);
            btn.add(&hb);
            let id = pane.id;
            let cb = Rc::clone(&on_focus);
            let r = self.root.clone();
            btn.connect_clicked(move |_| {
                r.hide();
                cb(id);
            });
            self.windows_box.pack_start(&btn, false, false, 0);
        }
        self.windows_box.show_all();
    }

    pub fn refresh_tiles(&self, snap: &PlatformSnapshot) {
        if let Some(ref lbl) = self.clock_label {
            let now = chrono::Local::now();
            lbl.set_text(&format!(
                "{}\n{}",
                now.format("%H:%M:%S"),
                now.format("%d/%m/%Y")
            ));
        }

        if let Some((ref img, ref lbl)) = self.audio_tile {
            let st = &snap.audio;
            if let Some(pb) = assets::pixbuf_from_bytes(audio_svg_for(st), 22, 22) {
                img.set_from_pixbuf(Some(&pb));
            }
            lbl.set_text(&if !st.available {
                "No audio".to_string()
            } else if st.muted {
                "Muted".to_string()
            } else {
                format!("Audio ({}%)", st.volume_percent)
            });
        }

        if let Some((ref img, ref lbl)) = self.battery_tile {
            let st = &snap.battery;
            let dyn_img = assets::dynamic_battery_image(
                st.percentage,
                st.charging,
                self.crit_thresh,
                self.low_thresh,
                22,
            );
            if let Some(pb) = dyn_img.pixbuf() {
                img.set_from_pixbuf(Some(&pb));
            }
            if st.charging {
                lbl.set_text(&format!("Plugged in,\ncharging ({}%)", st.percentage));
            } else if st.power_connected {
                lbl.set_text(&format!("Plugged in\n({}%)", st.percentage));
            } else {
                lbl.set_text(&format!("Battery\n({}%)", st.percentage));
            }
        }

        if let Some(ref lbl) = self.keyboard_label {
            let st = &snap.keyboard;
            lbl.set_text(if !st.current_long_name.is_empty() {
                &st.current_long_name
            } else if !st.current_short_name.is_empty() {
                &st.current_short_name
            } else {
                "Keyboard"
            });
        }

        if let Some((ref img, ref lbl)) = self.network_tile {
            let st = &snap.network;
            let svg = if !st.available {
                assets::ICON_WIFI_0_SVG
            } else if st.kind == 1 {
                assets::ICON_NETWORK_WIRED_SVG
            } else if st.status == 2 {
                let sig = st
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
            if let Some(pb) = assets::pixbuf_from_bytes(svg, 22, 22) {
                img.set_from_pixbuf(Some(&pb));
            }
            let text = if !st.available {
                "No network".to_string()
            } else if st.kind == 1 {
                "Wired".to_string()
            } else if st.status == 2 {
                st.networks
                    .iter()
                    .find(|n| n.active)
                    .map(|n| n.ssid.clone())
                    .unwrap_or_else(|| "Wi-Fi".to_string())
            } else {
                "Disconnected".to_string()
            };
            lbl.set_text(&text);
        }
    }
}

// =========================================================================
// Internal SEB Alt+Tab Window Switcher Overlay (AGENTS.md: SEB windows ONLY,
// still displays even when only 1 SEB window is open, cached thumbnails!)
// =========================================================================

pub struct WindowSwitcherOverlay {
    pub root: gtk::Box,
    grid: gtk::Grid,
    entries: RefCell<Vec<(u64, gtk::Box)>>,
    selected_index: Cell<usize>,
}

impl WindowSwitcherOverlay {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.style_context().add_class("seb-switcher-overlay");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_no_show_all(true);
        root.hide();

        let grid = gtk::Grid::new();
        grid.set_row_spacing(10);
        grid.set_column_spacing(10);
        root.pack_start(&grid, false, false, 0);

        Rc::new(Self {
            root,
            grid,
            entries: RefCell::new(Vec::new()),
            selected_index: Cell::new(0),
        })
    }

    pub fn step(
        &self,
        backwards: bool,
        panes: &[Rc<BrowserPane>],
        active_id: u64,
        on_commit_id: Rc<dyn Fn(u64)>,
    ) {
        if panes.is_empty() {
            self.root.hide();
            return;
        }

        if !self.root.is_visible() {
            self.rebuild_cards(panes, on_commit_id);
            let count = panes.len();
            if count == 1 {
                self.selected_index.set(0);
            } else {
                let active_idx = panes.iter().position(|p| p.id == active_id).unwrap_or(0);
                let next = if backwards {
                    (active_idx + count - 1) % count
                } else {
                    (active_idx + 1) % count
                };
                self.selected_index.set(next);
            }
            self.update_selection_styles();
            self.root.set_no_show_all(false);
            self.root.show_all();
            return;
        }

        let count = self.entries.borrow().len();
        if count == 0 {
            self.root.hide();
            return;
        }
        let cur = self.selected_index.get();
        let next = if backwards {
            (cur + count - 1) % count
        } else {
            (cur + 1) % count
        };
        self.selected_index.set(next);
        self.update_selection_styles();
    }

    pub fn commit(&self) -> Option<u64> {
        if !self.root.is_visible() {
            return None;
        }
        self.root.hide();
        let entries = self.entries.borrow();
        entries.get(self.selected_index.get()).map(|(id, _)| *id)
    }

    pub fn cancel(&self) {
        self.root.hide();
    }

    pub fn is_visible(&self) -> bool {
        self.root.is_visible()
    }

    fn rebuild_cards(&self, panes: &[Rc<BrowserPane>], on_commit_id: Rc<dyn Fn(u64)>) {
        for c in self.grid.children() {
            self.grid.remove(&c);
        }
        self.entries.borrow_mut().clear();

        for (i, pane) in panes.iter().enumerate() {
            let card = gtk::EventBox::new();
            card.set_visible_window(false);

            let card_frame = gtk::Box::new(gtk::Orientation::Vertical, 6);
            card_frame.style_context().add_class("seb-switcher-card");
            card_frame.set_size_request(CARD_WIDTH, CARD_HEIGHT);

            let hdr = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            hdr.pack_start(
                &assets::image_from_bytes(assets::ICON_SEB_PNG, 14, 14),
                false,
                false,
                0,
            );
            let title = pane.title();
            let short = if title.chars().count() > 26 {
                format!("{}...", title.chars().take(23).collect::<String>())
            } else {
                title
            };
            let title_lbl = gtk::Label::new(Some(&short));
            title_lbl.set_xalign(0.0);
            hdr.pack_start(&title_lbl, true, true, 0);
            card_frame.pack_start(&hdr, false, false, 0);

            let thumb_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
            thumb_box.style_context().add_class("seb-switcher-thumb");
            thumb_box.set_size_request(THUMB_WIDTH, THUMB_HEIGHT);
            if let Some(pb) = pane.preview_pixbuf(THUMB_WIDTH - 2, THUMB_HEIGHT - 2) {
                thumb_box.pack_start(&gtk::Image::from_pixbuf(Some(&pb)), true, true, 0);
            } else {
                let placeholder = gtk::Label::new(Some("Safe Exam Browser"));
                thumb_box.pack_start(&placeholder, true, true, 0);
            }
            card_frame.pack_start(&thumb_box, true, true, 0);

            card.add(&card_frame);
            let win_id = pane.id;
            let cb = Rc::clone(&on_commit_id);
            let root_c = self.root.clone();
            card.connect_button_press_event(move |_, ev| {
                if ev.button() == 1 {
                    root_c.hide();
                    cb(win_id);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });

            self.grid.attach(
                &card,
                (i as i32) % MAX_COLUMNS,
                (i as i32) / MAX_COLUMNS,
                1,
                1,
            );
            self.entries.borrow_mut().push((win_id, card_frame));
        }
    }

    fn update_selection_styles(&self) {
        let sel = self.selected_index.get();
        for (i, (_, card)) in self.entries.borrow().iter().enumerate() {
            let ctx = card.style_context();
            if i == sel {
                ctx.add_class("selected");
            } else {
                ctx.remove_class("selected");
            }
        }
    }
}
