use crate::assets;
use crate::clipboard::IsolatedClipboard;
use crate::dialogs;
use gtk::prelude::*;
use seb_config::settings::{
    AppSettings, ClipboardPolicy, PopupPolicy, ProxyPolicy, ProxyProtocol, UrlPolicy,
    WindowPosition, WindowSettings, SEB_APPLICATION_VERSION,
};
use seb_config::{calculate_browser_exam_key_hash, calculate_configuration_key_hash};
use seb_filter::{is_configuration_resource, FilterDecision, RequestFilter, ResourceType};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use url::Url;
use webkit2gtk::{
    AuthenticationRequestExt, Credential, CredentialPersistence, DeviceInfoPermissionRequest,
    FileChooserRequestExt, FindControllerExt, NavigationPolicyDecisionExt, PermissionRequestExt,
    PolicyDecisionExt, PolicyDecisionType, PrintOperation, PrintOperationExt,
    ResponsePolicyDecisionExt, ScriptDialogType, SettingsExt, URIRequestExt, URIResponseExt,
    UserContentInjectedFrames, UserContentManager, UserContentManagerExt,
    UserMediaPermissionRequest, UserMediaPermissionRequestExt, UserScript,
    UserScriptInjectionTime, WebContext, WebInspectorExt, WebView, WebViewExt,
};

pub const TITLE_BAR_HEIGHT: i32 = 26;
pub const RESIZE_BORDER: i32 = 6;
pub const MIN_WINDOW_WIDTH: i32 = 360;
pub const MIN_WINDOW_HEIGHT: i32 = 240;
pub const SNAP_EDGE_THRESHOLD: i32 = 2;
pub const SNAP_CORNER_THRESHOLD: i32 = 48;
const DRAG_START_THRESHOLD: i32 = 6;

const RESIZE_LEFT: u8 = 1 << 0;
const RESIZE_RIGHT: u8 = 1 << 1;
const RESIZE_TOP: u8 = 1 << 2;
const RESIZE_BOTTOM: u8 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapZone {
    None,
    Maximize,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, Copy)]
pub struct PaneRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone)]
pub enum PaneAction {
    Focus(u64),
    Close(u64),
    Minimize(u64),
    TitleOrUrlChanged { id: u64, title: String },
    OpenPopup { url: String },
    ReconfigureDownload { pane_id: u64, url: String },
    QuitUrlTriggered { reset: bool },
    SnapPreview(Option<PaneRect>),
    UserInteraction(String),
    BroadcastClipboard { id: String, b64: String },
    LmsUserIdentifierDetected(String),
    PageLoaded { url: String },
    OpenPdfInline { pane_id: u64, url: String },
    Log(String),
}

pub struct BrowserPane {
    pub id: u64,
    pub is_main: bool,
    pub root: gtk::EventBox,
    overlay: gtk::Overlay,
    frame_box: gtk::Box,
    title_event_box: gtk::EventBox,
    pub web_view: WebView,
    max_btn: gtk::Button,
    back_btn: Option<gtk::Button>,
    fwd_btn: Option<gtk::Button>,
    url_entry: Option<gtk::Entry>,
    toolbar_title_label: Option<gtk::Label>,
    zoom_pct_label: RefCell<Option<gtk::Button>>,
    zoom_overlay: gtk::EventBox,
    find_overlay: gtk::EventBox,
    find_entry: gtk::Entry,
    resize_strips: [gtk::EventBox; 4],
    pub maximized: Cell<bool>,
    pub snapped: Cell<bool>,
    pub minimized: Cell<bool>,
    floating_proctoring: Cell<bool>,
    pub restored_rect: RefCell<PaneRect>,
    pub current_rect: RefCell<PaneRect>,
    last_requested_size: Cell<(i32, i32)>,
    pub current_title: RefCell<String>,
    pub current_url: RefCell<String>,
    current_favicon: RefCell<Option<gdk_pixbuf::Pixbuf>>,
    zoom_percent: Cell<i32>,
    reload_confirm_active: Cell<bool>,
    last_preview: RefCell<Option<gdk_pixbuf::Pixbuf>>,
    window_settings: WindowSettings,
    home_url_target: String,
    home_requires_password: bool,
    home_password_hash: String,
    home_message: String,
    allow_print: bool,
}

impl BrowserPane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        is_main: bool,
        parent_window: &gtk::Window,
        workspace: &gtk::Layout,
        context: &WebContext,
        settings: &AppSettings,
        window_settings: WindowSettings,
        browser_exam_key: String,
        config_key: String,
        user_agent: String,
        home_url_target: String,
        upload_base_dir: PathBuf,
        clipboard: Rc<IsolatedClipboard>,
        initial_url: &str,
        on_action: Rc<dyn Fn(PaneAction)>,
    ) -> Rc<Self> {
        let root = gtk::EventBox::new();
        root.set_visible_window(true);
        root.style_context().add_class("seb-pane-host");
        let overlay = gtk::Overlay::new();
        root.add(&overlay);
        let frame_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame_box.style_context().add_class("seb-window-frame");
        overlay.add(&frame_box);

        // =====================================================================
        // 1. Custom 26px Title Bar (AGENTS.md + browser_window.cpp parity):
        //    Fixed "Safe Exam Browser" text, 3 buttons 34x26px:
        //    Minimize (−), Maximize/Restore (1-box/2-box), Close (×, disabled on main)
        // =====================================================================
        let title_event_box = gtk::EventBox::new();
        title_event_box.set_size_request(-1, TITLE_BAR_HEIGHT);
        title_event_box.set_above_child(false);
        title_event_box.add_events(
            gdk::EventMask::BUTTON_PRESS_MASK
                | gdk::EventMask::BUTTON_RELEASE_MASK
                | gdk::EventMask::BUTTON1_MOTION_MASK
                | gdk::EventMask::POINTER_MOTION_MASK,
        );

        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        title_row.style_context().add_class("seb-titlebar");
        title_row.set_size_request(-1, TITLE_BAR_HEIGHT);

        let icon_img = assets::image_from_bytes(assets::ICON_SEB_PNG, 15, 15);
        icon_img.set_margin_end(6);
        title_row.pack_start(&icon_img, false, false, 0);

        let title_label = gtk::Label::new(Some("Safe Exam Browser"));
        title_label.set_xalign(0.0);
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title_row.pack_start(&title_label, true, true, 0);

        // pack_end packs right-to-left: Close (far right), Maximize/Restore, Minimize
        let close_btn = gtk::Button::new();
        close_btn.style_context().add_class("seb-title-btn");
        close_btn.set_can_focus(false);
        close_btn.set_size_request(34, TITLE_BAR_HEIGHT);
        close_btn.set_image(Some(&assets::image_from_bytes(
            assets::close_icon_svg(),
            13,
            13,
        )));
        if is_main {
            close_btn.set_sensitive(false);
            close_btn.set_tooltip_text(Some("Main window cannot be closed"));
        } else {
            close_btn.set_tooltip_text(Some("Close Window"));
        }
        title_row.pack_end(&close_btn, false, false, 0);

        // Maximize / Restore button — ALWAYS enabled on main window too (AGENTS.md)!
        let max_btn = gtk::Button::new();
        max_btn.style_context().add_class("seb-title-btn");
        max_btn.set_can_focus(false);
        max_btn.set_size_request(34, TITLE_BAR_HEIGHT);
        max_btn.set_tooltip_text(Some(if is_main { "Restore" } else { "Maximize" }));
        max_btn.set_image(Some(&assets::image_from_bytes(
            if is_main {
                assets::restore_icon_svg()
            } else {
                assets::maximize_icon_svg()
            },
            14,
            14,
        )));
        title_row.pack_end(&max_btn, false, false, 0);

        let min_btn = gtk::Button::new();
        min_btn.style_context().add_class("seb-title-btn");
        min_btn.set_can_focus(false);
        min_btn.set_size_request(34, TITLE_BAR_HEIGHT);
        min_btn.set_tooltip_text(Some("Minimize"));
        min_btn.set_image(Some(&assets::image_from_bytes(
            assets::minimize_icon_svg(),
            13,
            13,
        )));
        title_row.pack_end(&min_btn, false, false, 0);

        title_event_box.add(&title_row);
        frame_box.pack_start(&title_event_box, false, false, 0);

        // =====================================================================
        // 2. Top Browser Toolbar (40px height, 22x22 icons, C++ visibility rule:
        //    show_toolbar || allow_address_bar || show_home_button || url_policy == Always;
        //    NO divider between title bar and toolbar when visible!)
        // =====================================================================
        let show_bar = window_settings.show_toolbar
            || window_settings.allow_address_bar
            || window_settings.show_home_button
            || window_settings.url_policy == UrlPolicy::Always;

        let mut url_entry_opt = None;
        let mut toolbar_title_opt = None;
        let back_btn_opt;
        let fwd_btn_opt;
        let reload_btn_opt;
        let home_btn_opt;
        let zoom_btn_opt;
        let find_btn_opt;

        if show_bar {
            let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            toolbar.style_context().add_class("seb-toolbar");
            toolbar.set_size_request(-1, 40);

            let mk_btn = |svg: &[u8], tip: &str| -> gtk::Button {
                let b = gtk::Button::new();
                b.style_context().add_class("seb-chrome-btn");
                b.set_can_focus(false);
                b.set_size_request(40, 38);
                b.set_tooltip_text(Some(tip));
                b.set_image(Some(&assets::image_from_bytes(svg, 22, 22)));
                b
            };

            if window_settings.allow_backward_navigation {
                let b = mk_btn(assets::ICON_BACK_SVG, "Back (Alt+Left)");
                b.set_sensitive(false);
                toolbar.pack_start(&b, false, false, 0);
                back_btn_opt = Some(b);
            } else {
                back_btn_opt = None;
            }

            if window_settings.allow_forward_navigation {
                let b = mk_btn(assets::ICON_FORWARD_SVG, "Forward (Alt+Right)");
                b.set_sensitive(false);
                toolbar.pack_start(&b, false, false, 0);
                fwd_btn_opt = Some(b);
            } else {
                fwd_btn_opt = None;
            }

            if window_settings.allow_reloading && window_settings.show_reload_button {
                let b = mk_btn(assets::ICON_RELOAD_SVG, "Reload (F5)");
                toolbar.pack_start(&b, false, false, 0);
                reload_btn_opt = Some(b);
            } else {
                reload_btn_opt = None;
            }

            if window_settings.show_home_button && !home_url_target.trim().is_empty() {
                let b = mk_btn(assets::ICON_HOME_SVG, "Home / Restart Exam (Alt+Home)");
                toolbar.pack_start(&b, false, false, 0);
                home_btn_opt = Some(b);
            } else {
                home_btn_opt = None;
            }

            if window_settings.allow_address_bar {
                let entry = gtk::Entry::new();
                entry.style_context().add_class("seb-url-entry");
                entry.set_placeholder_text(Some("Enter URL..."));
                entry.set_valign(gtk::Align::Center);
                toolbar.pack_start(&entry, true, true, 6);
                url_entry_opt = Some(entry);
            } else if window_settings.url_policy == UrlPolicy::Always {
                let entry = gtk::Entry::new();
                entry.style_context().add_class("seb-url-entry");
                entry.style_context().add_class("readonly");
                entry.set_editable(false);
                entry.set_can_focus(false);
                entry.set_text(initial_url);
                entry.set_valign(gtk::Align::Center);
                toolbar.pack_start(&entry, true, true, 6);
                url_entry_opt = Some(entry);
            } else {
                let lbl = gtk::Label::new(Some("Safe Exam Browser"));
                lbl.style_context().add_class("seb-toolbar-title");
                lbl.set_xalign(0.0);
                lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                toolbar.pack_start(&lbl, true, true, 8);
                toolbar_title_opt = Some(lbl);
            }

            if settings.browser.allow_page_zoom {
                let zb = mk_btn(assets::zoom_icon_svg(), "Page Zoom");
                toolbar.pack_start(&zb, false, false, 0);
                zoom_btn_opt = Some(zb);
            } else {
                zoom_btn_opt = None;
            }

            if settings.browser.allow_find {
                let fb = mk_btn(assets::search_icon_svg(), "Find in Page (Ctrl+F)");
                toolbar.pack_start(&fb, false, false, 0);
                find_btn_opt = Some(fb);
            } else {
                find_btn_opt = None;
            }

            frame_box.pack_start(&toolbar, false, false, 0);
        } else {
            back_btn_opt = None;
            fwd_btn_opt = None;
            reload_btn_opt = None;
            home_btn_opt = None;
            zoom_btn_opt = None;
            find_btn_opt = None;
        }

        // Single hairline divider below title bar + optional toolbar
        let divider = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        divider.style_context().add_class("seb-hairline");
        divider.set_size_request(-1, 1);
        frame_box.pack_start(&divider, false, false, 0);

        let block_middle_entry = |entry: &gtk::Entry| {
            if !settings.mouse.allow_middle_button
                || settings.security.clipboard_policy != ClipboardPolicy::Allow
            {
                entry.connect_button_press_event(|_, ev| {
                    if ev.button() == 2 {
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                });
            }
        };
        if let Some(ref e) = url_entry_opt {
            block_middle_entry(e);
        }

        // Floating Find & Zoom overlay cards matching C++ `BrowserWindow::toggleFindPopup` /
        // `toggleZoomPopup` (pure sharp rectangular #f0f0f0 cards with 1px gray border, zero GTK arrow tips!).
        let find_overlay = gtk::EventBox::new();
        find_overlay.set_visible_window(false);
        find_overlay.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
        let find_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        find_row.style_context().add_class("seb-popup");
        find_row.style_context().add_class("seb-frame-popup");
        find_row.style_context().add_class("seb-find-popup");
        let find_entry = gtk::Entry::new();
        find_entry.style_context().add_class("seb-url-entry");
        find_entry.set_placeholder_text(Some("Find"));
        find_entry.set_size_request(210, 30);
        find_entry.set_valign(gtk::Align::Center);
        block_middle_entry(&find_entry);
        let find_prev = gtk::Button::with_label("▲");
        find_prev.style_context().add_class("seb-chrome-btn");
        find_prev.set_can_focus(false);
        find_prev.set_size_request(28, 30);
        find_prev.set_tooltip_text(Some("Previous match"));
        let find_next = gtk::Button::with_label("▼");
        find_next.style_context().add_class("seb-chrome-btn");
        find_next.set_can_focus(false);
        find_next.set_size_request(28, 30);
        find_next.set_tooltip_text(Some("Next match"));
        let find_case = gtk::CheckButton::with_label("Aa");
        find_case.set_can_focus(false);
        find_case.set_tooltip_text(Some("Match case (Case Sensitive)"));
        let find_close = gtk::Button::with_label("×");
        find_close.style_context().add_class("seb-chrome-btn");
        find_close.set_can_focus(false);
        find_close.set_size_request(30, 30);
        find_row.pack_start(&find_entry, false, false, 0);
        find_row.pack_start(&find_prev, false, false, 0);
        find_row.pack_start(&find_next, false, false, 0);
        find_row.pack_start(&find_case, false, false, 2);
        find_row.pack_start(&find_close, false, false, 0);
        find_overlay.add(&find_row);
        find_overlay.show_all();
        find_overlay.set_no_show_all(true);
        find_overlay.hide();

        let zoom_overlay = gtk::EventBox::new();
        zoom_overlay.set_visible_window(false);
        zoom_overlay.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
        let zoom_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        zoom_row.style_context().add_class("seb-popup");
        zoom_row.style_context().add_class("seb-frame-popup");
        let zoom_minus = gtk::Button::with_label("−");
        zoom_minus.style_context().add_class("seb-chrome-btn");
        zoom_minus.set_can_focus(false);
        zoom_minus.set_size_request(34, 32);
        let zoom_pct_btn = gtk::Button::with_label("100%");
        zoom_pct_btn.style_context().add_class("seb-chrome-btn");
        zoom_pct_btn.set_can_focus(false);
        zoom_pct_btn.set_size_request(62, 32);
        zoom_pct_btn.set_tooltip_text(Some("Reset Zoom (100%)"));
        let zoom_plus = gtk::Button::with_label("+");
        zoom_plus.style_context().add_class("seb-chrome-btn");
        zoom_plus.set_can_focus(false);
        zoom_plus.set_size_request(34, 32);
        zoom_row.pack_start(&zoom_minus, false, false, 0);
        zoom_row.pack_start(&zoom_pct_btn, false, false, 0);
        zoom_row.pack_start(&zoom_plus, false, false, 0);
        zoom_overlay.add(&zoom_row);
        zoom_overlay.show_all();
        zoom_overlay.set_no_show_all(true);
        zoom_overlay.hide();

        // =====================================================================
        // 3. WebKitGTK 4.1 WebView + AllFrames JS API + Clipboard + PageZoom
        // =====================================================================
        let ucm = UserContentManager::new();
        let init_bek = if !browser_exam_key.is_empty() {
            calculate_browser_exam_key_hash(&browser_exam_key, initial_url)
        } else {
            String::new()
        };
        let init_ck = if !config_key.is_empty() {
            calculate_configuration_key_hash(&config_key, initial_url)
        } else {
            String::new()
        };
        let api_js = assets::SEB_API_JS_TEMPLATE
            .replace("%%SEB_VERSION%%", &format!("SEB_Linux_{SEB_APPLICATION_VERSION}"))
            .replace("%%INITIAL_BEK%%", &init_bek)
            .replace("%%INITIAL_CK%%", &init_ck);

        // Inject SEB JS API on ALL frames (parity with C++ setRunsOnSubFrames(true))
        ucm.add_script(&UserScript::new(
            &api_js,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
        ucm.register_script_message_handler("sebSecurity");

        // Wheel/pinch zoom blocker when !allow_page_zoom
        if !settings.browser.allow_page_zoom {
            ucm.add_script(&UserScript::new(
                assets::PAGE_ZOOM_BLOCK_JS,
                UserContentInjectedFrames::AllFrames,
                UserScriptInjectionTime::Start,
                &[],
                &[],
            ));
        }

        // Clipboard policy JS injection on ALL frames
        match settings.security.clipboard_policy {
            ClipboardPolicy::Block => {
                ucm.add_script(&UserScript::new(
                    assets::CLIPBOARD_BLOCK_JS,
                    UserContentInjectedFrames::AllFrames,
                    UserScriptInjectionTime::Start,
                    &[],
                    &[],
                ));
            }
            ClipboardPolicy::Isolated => {
                ucm.add_script(&UserScript::new(
                    assets::CLIPBOARD_ISOLATED_JS,
                    UserContentInjectedFrames::AllFrames,
                    UserScriptInjectionTime::Start,
                    &[],
                    &[],
                ));
                ucm.register_script_message_handler("sebClipboard");
            }
            ClipboardPolicy::Allow => {}
        }

        let web_view = WebView::builder()
            .web_context(context)
            .user_content_manager(&ucm)
            .build();

        // Block Drag-and-Drop onto WebView (parity with C++ qt_webengine_view & webkitgtk_view)
        web_view.drag_dest_unset();
        web_view.connect_drag_drop(|_, ctx, _, _, time| {
            ctx.drop_finish(false, time);
            true
        });

        if let Some(wk_settings) = WebViewExt::settings(&web_view) {
            if !user_agent.is_empty() {
                wk_settings.set_user_agent(Some(&user_agent));
            }
            wk_settings.set_enable_developer_extras(window_settings.allow_developer_console);
            wk_settings.set_javascript_can_open_windows_automatically(
                settings.browser.popup_policy != PopupPolicy::Block,
            );
            wk_settings.set_enable_javascript(true);
            wk_settings.set_allow_file_access_from_file_urls(false);
            wk_settings.set_allow_universal_access_from_file_urls(false);
            wk_settings.set_javascript_can_access_clipboard(
                settings.security.clipboard_policy == ClipboardPolicy::Allow,
            );
            let allow_any_media = settings.browser.media_capture_camera
                || settings.browser.media_capture_microphone;
            wk_settings.set_enable_media_stream(allow_any_media);
            wk_settings.set_enable_webrtc(allow_any_media);
            wk_settings.set_enable_html5_database(true);
            wk_settings.set_enable_html5_local_storage(true);
        }

        // JS `window.SafeExamBrowser.security.updateKeys(cb)` message handler
        {
            let wv_weak = web_view.downgrade();
            let bek = browser_exam_key.clone();
            let ck = config_key.clone();
            ucm.connect_script_message_received(Some("sebSecurity"), move |_mgr, js_res| {
                let Some(wv) = wv_weak.upgrade() else { return };
                let Some(js_val) = js_res.js_value() else { return };
                let payload = js_val.to_string();
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    return;
                };
                let req_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("0");
                let url = wv
                    .uri()
                    .map(|u| u.to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| v.get("url").and_then(|x| x.as_str()).map(|s| s.to_string()))
                    .unwrap_or_default();
                let bek_hash = if !bek.is_empty() {
                    calculate_browser_exam_key_hash(&bek, &url)
                } else {
                    String::new()
                };
                let ck_hash = if !ck.is_empty() {
                    calculate_configuration_key_hash(&ck, &url)
                } else {
                    String::new()
                };
                let script = format!(
                    "window.__sebApplyKeys && window.__sebApplyKeys({}, {}, {});",
                    serde_json::to_string(req_id).unwrap_or_else(|_| "\"\"".into()),
                    serde_json::to_string(&bek_hash).unwrap_or_else(|_| "\"\"".into()),
                    serde_json::to_string(&ck_hash).unwrap_or_else(|_| "\"\"".into()),
                );
                wv.run_javascript(&script, gio::Cancellable::NONE, |_| {});
            });
        }

        // Isolated Clipboard cross-window message handler
        if settings.security.clipboard_policy == ClipboardPolicy::Isolated {
            let cb = Rc::clone(&clipboard);
            let act = Rc::clone(&on_action);
            ucm.connect_script_message_received(Some("sebClipboard"), move |_mgr, js_res| {
                let Some(js_val) = js_res.js_value() else { return };
                let payload = js_val.to_string();
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    return;
                };
                let cid = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
                let b64 = v.get("content").and_then(|x| x.as_str()).unwrap_or("");
                if let Some((accepted_id, accepted_b64)) = cb.accept_js_update(cid, b64) {
                    act(PaneAction::BroadcastClipboard {
                        id: accepted_id,
                        b64: accepted_b64,
                    });
                }
            });
        }

        let web_overlay = gtk::Overlay::new();
        web_overlay.add(&web_view);

        // Single unified 2px page-loading progress bar directly under topbar/titlebar (SEB #2196F3)
        let load_fraction = Rc::new(Cell::new(0.0f64));
        let load_progress = gtk::DrawingArea::new();
        load_progress.set_halign(gtk::Align::Fill);
        load_progress.set_valign(gtk::Align::Start);
        load_progress.set_hexpand(true);
        load_progress.set_size_request(-1, 2);
        load_progress.set_no_show_all(true);
        load_progress.hide();
        {
            let frac_c = Rc::clone(&load_fraction);
            load_progress.connect_draw(move |da, cr| {
                let frac = frac_c.get().clamp(0.0, 1.0);
                let w = da.allocated_width() as f64;
                let h = da.allocated_height() as f64;
                if frac > 0.0 && w > 0.0 && h > 0.0 {
                    cr.set_source_rgb(0x21 as f64 / 255.0, 0x96 as f64 / 255.0, 0xF3 as f64 / 255.0);
                    cr.rectangle(0.0, 0.0, w * frac, h);
                    let _ = cr.fill();
                }
                glib::Propagation::Proceed
            });
        }
        web_overlay.add_overlay(&load_progress);
        web_overlay.set_overlay_pass_through(&load_progress, true);

        frame_box.pack_start(&web_overlay, true, true, 0);

        // =====================================================================
        // 4. 8-Edge/Corner Interactive Resize Strips (6px border, all 4 cursors)
        // =====================================================================
        let mk_strip = |halign: gtk::Align, valign: gtk::Align, w: i32, h: i32| -> gtk::EventBox {
            let eb = gtk::EventBox::new();
            eb.set_visible_window(false);
            eb.set_halign(halign);
            eb.set_valign(valign);
            eb.set_size_request(w, h);
            eb.add_events(
                gdk::EventMask::BUTTON_PRESS_MASK
                    | gdk::EventMask::BUTTON_RELEASE_MASK
                    | gdk::EventMask::POINTER_MOTION_MASK
                    | gdk::EventMask::ENTER_NOTIFY_MASK
                    | gdk::EventMask::LEAVE_NOTIFY_MASK,
            );
            eb.set_no_show_all(true);
            eb.hide();
            overlay.add_overlay(&eb);
            eb
        };
        let strip_left = mk_strip(gtk::Align::Start, gtk::Align::Fill, RESIZE_BORDER, -1);
        let strip_right = mk_strip(gtk::Align::End, gtk::Align::Fill, RESIZE_BORDER, -1);
        let strip_top = mk_strip(gtk::Align::Fill, gtk::Align::Start, -1, 3);
        strip_top.set_margin_end(34 * 3);
        let strip_bottom = mk_strip(gtk::Align::Fill, gtk::Align::End, -1, RESIZE_BORDER);
        let resize_strips = [strip_left, strip_right, strip_top, strip_bottom];

        overlay.add_overlay(&zoom_overlay);
        overlay.add_overlay(&find_overlay);
        {
            let find_w: gtk::Widget = find_overlay.clone().upcast();
            let zoom_w: gtk::Widget = zoom_overlay.clone().upcast();
            let find_anchor = find_btn_opt.clone();
            let zoom_anchor = zoom_btn_opt.clone();
            overlay.connect_get_child_position(move |ov, child| {
                let target_anchor = if child == &find_w {
                    Some(&find_anchor)
                } else if child == &zoom_w {
                    Some(&zoom_anchor)
                } else {
                    None
                };
                let anchor_opt = target_anchor?;
                let (_, nat) = child.preferred_size();
                let w = nat.width.max(1);
                let h = nat.height.max(1);
                let ov_w = ov.allocated_width().max(w + 16);
                if let Some(ref btn) = anchor_opt {
                    if btn.is_visible() {
                        if let Some((bx, by)) = btn.translate_coordinates(ov, 0, 0) {
                            let bw = btn.allocated_width();
                            let bh = btn.allocated_height();
                            let x = (bx + bw - w).clamp(4, (ov_w - w - 4).max(4));
                            let y = by + bh + 2;
                            return Some(gdk::Rectangle::new(x, y, w, h));
                        }
                    }
                }
                let x = (ov_w - w - 12).max(4);
                let y = TITLE_BAR_HEIGHT + 8;
                Some(gdk::Rectangle::new(x, y, w, h))
            });
        }

        let pane = Rc::new(Self {
            id,
            is_main,
            root: root.clone(),
            overlay: overlay.clone(),
            frame_box: frame_box.clone(),
            title_event_box: title_event_box.clone(),
            web_view: web_view.clone(),
            max_btn: max_btn.clone(),
            back_btn: back_btn_opt.clone(),
            fwd_btn: fwd_btn_opt.clone(),
            url_entry: url_entry_opt,
            toolbar_title_label: toolbar_title_opt,
            zoom_pct_label: RefCell::new(Some(zoom_pct_btn.clone())),
            zoom_overlay: zoom_overlay.clone(),
            find_overlay: find_overlay.clone(),
            find_entry: find_entry.clone(),
            resize_strips,
            maximized: Cell::new(is_main),
            snapped: Cell::new(false),
            minimized: Cell::new(false),
            floating_proctoring: Cell::new(false),
            restored_rect: RefCell::new(PaneRect {
                x: 40,
                y: 30,
                w: 960,
                h: 640,
            }),
            current_rect: RefCell::new(PaneRect {
                x: 0,
                y: 0,
                w: 960,
                h: 640,
            }),
            last_requested_size: Cell::new((0, 0)),
            current_title: RefCell::new("Safe Exam Browser".to_string()),
            current_url: RefCell::new(initial_url.to_string()),
            current_favicon: RefCell::new(None),
            zoom_percent: Cell::new(100),
            reload_confirm_active: Cell::new(false),
            last_preview: RefCell::new(None),
            window_settings: window_settings.clone(),
            home_url_target,
            home_requires_password: settings.browser.home_navigation_requires_password,
            home_password_hash: settings.browser.home_password_hash.clone(),
            home_message: settings.browser.home_navigation_message.clone(),
            allow_print: settings.browser.allow_print,
        });

        // Wire 8-edge interactive resize handles
        pane.wire_resize_handles(workspace, &on_action);

        // Wire toolbar controls
        if let Some(btn) = back_btn_opt {
            let wv = web_view.clone();
            btn.connect_clicked(move |_| wv.go_back());
        }
        if let Some(btn) = fwd_btn_opt {
            let wv = web_view.clone();
            btn.connect_clicked(move |_| wv.go_forward());
        }
        if let Some(btn) = reload_btn_opt {
            let p = Rc::clone(&pane);
            btn.connect_clicked(move |_| p.reload());
        }
        if let Some(btn) = home_btn_opt {
            let p = Rc::clone(&pane);
            btn.connect_clicked(move |_| p.navigate_home());
        }
        if let Some(ref entry) = pane.url_entry {
            if window_settings.allow_address_bar {
                let wv = web_view.clone();
                entry.connect_activate(move |e| {
                    let raw = e.text().trim().to_string();
                    if raw.is_empty() {
                        return;
                    }
                    let target = normalize_entered_url(&raw);
                    wv.load_uri(&target);
                });
            }
        }
        if let Some(btn) = zoom_btn_opt {
            let p = Rc::clone(&pane);
            btn.connect_clicked(move |_| p.toggle_zoom_popup());
        }
        {
            let p = Rc::clone(&pane);
            zoom_minus.connect_clicked(move |_| p.adjust_zoom_steps(-1));
        }
        {
            let p = Rc::clone(&pane);
            zoom_pct_btn.connect_clicked(move |_| p.reset_zoom());
        }
        {
            let p = Rc::clone(&pane);
            zoom_plus.connect_clicked(move |_| p.adjust_zoom_steps(1));
        }
        if let Some(btn) = find_btn_opt {
            let p = Rc::clone(&pane);
            btn.connect_clicked(move |_| p.toggle_find_bar());
        }

        // Find popup wiring (live search, Case Sensitive toggle, Prev/Next, Enter -> next, Esc / close -> close & clear)
        {
            let run_search = {
                let wv = web_view.clone();
                let entry = find_entry.clone();
                let case_cb = find_case.clone();
                Rc::new(move || {
                    if let Some(fc) = wv.find_controller() {
                        let text = entry.text();
                        if text.is_empty() {
                            fc.search_finish();
                        } else {
                            // WebKitFindOptions: CASE_INSENSITIVE = 1 << 0, WRAP_AROUND = 1 << 4 (16)
                            let opts: u32 = if case_cb.is_active() { 16 } else { 1 | 16 };
                            fc.search(text.as_str(), opts, u32::MAX);
                        }
                    }
                })
            };
            {
                let rs = Rc::clone(&run_search);
                find_entry.connect_changed(move |_| rs());
            }
            {
                let rs = Rc::clone(&run_search);
                find_case.connect_toggled(move |_| rs());
            }
            {
                let wv = web_view.clone();
                find_prev.connect_clicked(move |_| {
                    if let Some(fc) = wv.find_controller() {
                        fc.search_previous();
                    }
                });
            }
            {
                let wv = web_view.clone();
                find_next.connect_clicked(move |_| {
                    if let Some(fc) = wv.find_controller() {
                        fc.search_next();
                    }
                });
            }
            let wv = web_view.clone();
            find_entry.connect_activate(move |_| {
                if let Some(fc) = wv.find_controller() {
                    fc.search_next();
                }
            });
            let p = Rc::clone(&pane);
            find_entry.connect_key_press_event(move |_, ev| {
                if ev.keyval() == gdk::keys::constants::Escape {
                    p.close_find_bar();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            let p = Rc::clone(&pane);
            find_close.connect_clicked(move |_| p.close_find_bar());
        }

        // Title bar buttons: Minimize, Maximize/Restore, Close
        {
            let act = Rc::clone(&on_action);
            min_btn.connect_clicked(move |_| act(PaneAction::Minimize(id)));
        }
        {
            let p = Rc::clone(&pane);
            let ws = workspace.clone();
            max_btn.connect_clicked(move |_| p.toggle_maximize(&ws));
        }
        if !is_main {
            let act = Rc::clone(&on_action);
            close_btn.connect_clicked(move |_| act(PaneAction::Close(id)));
        }

        // Title-bar drag + double-click maximize + proportional unmaximize + Aero Snap!
        {
            let p = Rc::clone(&pane);
            let ws = workspace.clone();
            let act = Rc::clone(&on_action);
            #[derive(Default)]
            struct DragState {
                active: bool,
                dragging: bool,
                start_rx: f64,
                start_ry: f64,
                local_x: f64,
                orig_x: i32,
                orig_y: i32,
                snap: Option<SnapZone>,
                last_drag_finished: Option<std::time::Instant>,
            }
            let drag = Rc::new(RefCell::new(DragState::default()));

            let d_press = Rc::clone(&drag);
            let p_press = Rc::clone(&p);
            let ws_press = ws.clone();
            let act_press = Rc::clone(&act);
            title_event_box.connect_button_press_event(move |_, ev| {
                if ev.button() != 1 {
                    act_press(PaneAction::Focus(id));
                    return glib::Propagation::Proceed;
                }
                act_press(PaneAction::SnapPreview(None));
                if ev.event_type() == gdk::EventType::DoubleButtonPress {
                    let recent_drag = d_press
                        .borrow()
                        .last_drag_finished
                        .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(650));
                    if !recent_drag {
                        let mut st = d_press.borrow_mut();
                        st.active = false;
                        st.dragging = false;
                        st.snap = None;
                        drop(st);
                        act_press(PaneAction::Focus(id));
                        p_press.toggle_maximize(&ws_press);
                        return glib::Propagation::Stop;
                    }
                }
                let (lx, _) = ev.position();
                let (wx, wy, _) = workspace_pointer(&ws_press, ev.device().as_ref(), ev.root());
                let cur = *p_press.current_rect.borrow();
                {
                    let mut st = d_press.borrow_mut();
                    st.active = true;
                    st.dragging = false;
                    st.start_rx = wx;
                    st.start_ry = wy;
                    st.local_x = lx;
                    st.orig_x = cur.x;
                    st.orig_y = cur.y;
                    st.snap = None;
                }
                act_press(PaneAction::Focus(id));
                glib::Propagation::Stop
            });

            let d_move = Rc::clone(&drag);
            let p_move = Rc::clone(&p);
            let ws_move = ws.clone();
            let act_move = Rc::clone(&act);
            title_event_box.connect_motion_notify_event(move |_, ev| {
                let (rx, ry, mask_opt) =
                    workspace_pointer(&ws_move, ev.device().as_ref(), ev.root());
                let btn_down = ev.state().contains(gdk::ModifierType::BUTTON1_MASK)
                    || mask_opt.is_some_and(|m| m.contains(gdk::ModifierType::BUTTON1_MASK));
                let ws_w = ws_move.allocated_width().max(MIN_WINDOW_WIDTH);
                let ws_h = ws_move.allocated_height().max(MIN_WINDOW_HEIGHT);

                let (unmax_rect, move_rect, zone_opt) = {
                    let mut st = d_move.borrow_mut();
                    if !st.active {
                        return glib::Propagation::Proceed;
                    }
                    if !btn_down {
                        if st.dragging {
                            st.last_drag_finished = Some(std::time::Instant::now());
                        }
                        st.active = false;
                        st.dragging = false;
                        st.snap = None;
                        drop(st);
                        act_move(PaneAction::SnapPreview(None));
                        return glib::Propagation::Proceed;
                    }
                    let dx = (rx - st.start_rx) as i32;
                    let dy = (ry - st.start_ry) as i32;
                    if !st.dragging
                        && dx.abs() < DRAG_START_THRESHOLD
                        && dy.abs() < DRAG_START_THRESHOLD
                    {
                        return glib::Propagation::Stop;
                    }
                    st.dragging = true;

                    // Unmaximize/unsnap with proportional horizontal grab ratio
                    let mut unmax_rect = None;
                    let cur_w_for_clamp = if p_move.maximized.get() || p_move.snapped.get() {
                        let cur_rect = *p_move.current_rect.borrow();
                        let cur_w = cur_rect.w.max(1) as f64;
                        let ratio = (st.local_x / cur_w).clamp(0.1, 0.9);
                        let mut restored = *p_move.restored_rect.borrow();
                        if restored.w < MIN_WINDOW_WIDTH || restored.w >= ws_w {
                            restored.w = (ws_w * 80 / 100).max(480).min(ws_w);
                            restored.h = (ws_h * 80 / 100).max(360).min(ws_h);
                        }
                        let new_x = (rx as i32 - (restored.w as f64 * ratio) as i32)
                            .clamp(-restored.w + 80, ws_w - 80);
                        let new_y = cur_rect.y.clamp(0, (ws_h - TITLE_BAR_HEIGHT).max(0));
                        st.orig_x = new_x;
                        st.orig_y = new_y;
                        st.start_rx = rx;
                        st.start_ry = ry;
                        let r = PaneRect {
                            x: new_x,
                            y: new_y,
                            w: restored.w,
                            h: restored.h,
                        };
                        unmax_rect = Some(r);
                        (r.w, r.h)
                    } else {
                        let cur = *p_move.current_rect.borrow();
                        (cur.w, cur.h)
                    };

                    let nx = (st.orig_x + (rx - st.start_rx) as i32)
                        .clamp(-cur_w_for_clamp.0 + 80, ws_w - 80);
                    let ny = (st.orig_y + (ry - st.start_ry) as i32)
                        .clamp(0, (ws_h - TITLE_BAR_HEIGHT).max(0));
                    let move_rect = PaneRect {
                        x: nx,
                        y: ny,
                        w: cur_w_for_clamp.0,
                        h: cur_w_for_clamp.1,
                    };

                    let zone = detect_snap_zone(rx as i32, ry as i32, ws_w, ws_h);
                    st.snap = Some(zone);
                    (unmax_rect, move_rect, snap_rect_for_zone(zone, ws_w, ws_h))
                };

                if let Some(r) = unmax_rect {
                    p_move.set_windowed_rect(&ws_move, r, false);
                }
                p_move.set_windowed_rect(&ws_move, move_rect, false);
                act_move(PaneAction::SnapPreview(zone_opt));
                glib::Propagation::Stop
            });

            let d_rel = Rc::clone(&drag);
            let p_rel = Rc::clone(&p);
            let ws_rel = ws.clone();
            let act_rel = Rc::clone(&act);
            title_event_box.connect_button_release_event(move |_, _| {
                let snap_opt = {
                    let mut st = d_rel.borrow_mut();
                    if st.active {
                        let was_dragging = st.dragging;
                        let snap = st.snap.take().unwrap_or(SnapZone::None);
                        st.active = false;
                        st.dragging = false;
                        if was_dragging {
                            st.last_drag_finished = Some(std::time::Instant::now());
                        }
                        Some(snap)
                    } else {
                        None
                    }
                };
                if let Some(snap) = snap_opt {
                    act_rel(PaneAction::SnapPreview(None));
                    let ws_w = ws_rel.allocated_width().max(MIN_WINDOW_WIDTH);
                    let ws_h = ws_rel.allocated_height().max(MIN_WINDOW_HEIGHT);
                    match snap {
                        SnapZone::None => {}
                        SnapZone::Maximize => p_rel.maximize(&ws_rel),
                        other => {
                            if let Some(r) = snap_rect_for_zone(other, ws_w, ws_h) {
                                p_rel.set_windowed_rect(&ws_rel, r, true);
                            }
                        }
                    }
                }
                glib::Propagation::Stop
            });

            let d_brk = Rc::clone(&drag);
            let act_brk = Rc::clone(&act);
            title_event_box.connect_grab_broken_event(move |_, _| {
                {
                    let mut st = d_brk.borrow_mut();
                    if st.dragging {
                        st.last_drag_finished = Some(std::time::Instant::now());
                    }
                    st.active = false;
                    st.dragging = false;
                    st.snap = None;
                }
                act_brk(PaneAction::SnapPreview(None));
                glib::Propagation::Proceed
            });
        }

        // Title & URL change listeners + Back/Forward sensitivity sync + UrlPolicy::BeforeTitle
        {
            let p = Rc::clone(&pane);
            let act = Rc::clone(&on_action);
            web_view.connect_title_notify(move |wv| {
                let title = wv
                    .title()
                    .map(|t| t.to_string())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "Safe Exam Browser".to_string());
                *p.current_title.borrow_mut() = title.clone();
                p.sync_navigation_chrome();
                act(PaneAction::TitleOrUrlChanged { id, title });
            });
        }
        {
            let p = Rc::clone(&pane);
            let act = Rc::clone(&on_action);
            let bek = browser_exam_key.clone();
            let ck = config_key.clone();
            web_view.connect_uri_notify(move |wv| {
                let uri = wv.uri().map(|u| u.to_string()).unwrap_or_default();
                if !uri.is_empty() {
                    let bek_hash = if !bek.is_empty() {
                        calculate_browser_exam_key_hash(&bek, &uri)
                    } else {
                        String::new()
                    };
                    let ck_hash = if !ck.is_empty() {
                        calculate_configuration_key_hash(&ck, &uri)
                    } else {
                        String::new()
                    };
                    let script = format!(
                        "window.__sebApplyKeys && window.__sebApplyKeys('nav', {}, {});",
                        serde_json::to_string(&bek_hash).unwrap_or_else(|_| "\"\"".into()),
                        serde_json::to_string(&ck_hash).unwrap_or_else(|_| "\"\"".into()),
                    );
                    wv.run_javascript(&script, gio::Cancellable::NONE, |_| {});
                }
                *p.current_url.borrow_mut() = uri;
                p.sync_navigation_chrome();
                let title = p.current_title.borrow().clone();
                act(PaneAction::TitleOrUrlChanged { id, title });
            });
        }
        // Track webpage favicon for Action Center sidebar & Alt+Tab switcher cards
        {
            let p = Rc::clone(&pane);
            let act = Rc::clone(&on_action);
            web_view.connect_favicon_notify(move |wv| {
                let pb = wv.favicon().and_then(surface_to_pixbuf);
                *p.current_favicon.borrow_mut() = pb;
                let title = p.current_title.borrow().clone();
                act(PaneAction::TitleOrUrlChanged { id, title });
            });
        }

        // Unified 2px #2196F3 page-loading progress bar wiring
        {
            let load_gen = Rc::new(Cell::new(0u64));
            let set_progress = {
                let pb = load_progress.clone();
                let frac_c = Rc::clone(&load_fraction);
                move |frac: f64| {
                    frac_c.set(frac.clamp(0.0, 1.0));
                    pb.show();
                    pb.queue_draw();
                }
            };
            {
                let set_p = set_progress.clone();
                let gen_c = Rc::clone(&load_gen);
                let pb = load_progress.clone();
                let frac_c = Rc::clone(&load_fraction);
                let p_snap = Rc::clone(&pane);
                let act_load = Rc::clone(&on_action);
                web_view.connect_load_changed(move |wv, ev| match ev {
                    webkit2gtk::LoadEvent::Started
                    | webkit2gtk::LoadEvent::Redirected
                    | webkit2gtk::LoadEvent::Committed => {
                        gen_c.set(gen_c.get().wrapping_add(1));
                        set_p(wv.estimated_load_progress().clamp(0.08, 0.98));
                    }
                    webkit2gtk::LoadEvent::Finished => {
                        let gen = gen_c.get().wrapping_add(1);
                        gen_c.set(gen);
                        set_p(1.0);
                        let pb_hide = pb.clone();
                        let gen_hide = Rc::clone(&gen_c);
                        let frac_hide = Rc::clone(&frac_c);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(200),
                            move || {
                                if gen_hide.get() == gen {
                                    frac_hide.set(0.0);
                                    pb_hide.hide();
                                }
                            },
                        );
                        if let Some(uri) = wv.uri() {
                            act_load(PaneAction::PageLoaded {
                                url: uri.to_string(),
                            });
                        }
                        // Pre-cache live WebView snapshot shortly after page finishes painting
                        let p_c = Rc::clone(&p_snap);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(250),
                            move || {
                                p_c.refresh_preview_async(202, 112, None);
                            },
                        );
                    }
                    _ => {}
                });
            }
            {
                let set_p = set_progress;
                let gen_c = Rc::clone(&load_gen);
                web_view.connect_estimated_load_progress_notify(move |wv| {
                    if wv.is_loading() {
                        let frac = wv.estimated_load_progress();
                        if frac < 1.0 {
                            gen_c.set(gen_c.get().wrapping_add(1));
                            set_p(frac.clamp(0.08, 0.99));
                        }
                    }
                });
            }
            {
                let pb = load_progress;
                let gen_c = load_gen;
                let frac_c = load_fraction;
                web_view.connect_load_failed(move |_, _, _, _| {
                    gen_c.set(gen_c.get().wrapping_add(1));
                    frac_c.set(0.0);
                    pb.hide();
                    false
                });
            }
        }

        // Navigation, response (authenticated .seb download via WebKitGTK!), PDF, & popup policy
        {
            let filter = Rc::new(RequestFilter::new(&seb_filter::FilterConfig {
                process_main_requests: settings.browser.filter.process_main_requests,
                process_content_requests: settings.browser.filter.process_content_requests,
                rules: settings.browser.filter.rules.clone(),
            }));
            let quit_url = settings.browser.quit_url.trim().to_string();
            let reset_on_quit = settings.browser.reset_on_quit_url;
            let allow_reconfig = settings.security.allow_reconfiguration
                && settings.browser.allow_configuration_downloads;
            let allow_pdf = settings.browser.allow_pdf_reader;
            let popup_policy = settings.browser.popup_policy;
            let act = Rc::clone(&on_action);
            let p_pol = Rc::clone(&pane);

            web_view.connect_decide_policy(move |wv, decision, dtype| {
                match dtype {
                    PolicyDecisionType::NavigationAction => {
                        if let Some(nav) = decision.downcast_ref::<webkit2gtk::NavigationPolicyDecision>() {
                            if let Some(action) = nav.navigation_action() {
                                if let Some(req) = action.request() {
                                    if let Some(uri_gstr) = req.uri() {
                                        let uri = uri_gstr.as_str();
                                        let lower = uri.to_ascii_lowercase();

                                        // Block file:// and mailto: unconditionally (parity!)
                                        if lower.starts_with("file://") || lower.starts_with("mailto:") {
                                            decision.ignore();
                                            return true;
                                        }

                                        // seb:// or sebs:// link -> reconfigure!
                                        if lower.starts_with("seb://") || lower.starts_with("sebs://") {
                                            decision.ignore();
                                            if allow_reconfig {
                                                act(PaneAction::ReconfigureDownload {
                                                    pane_id: id,
                                                    url: uri.to_string(),
                                                });
                                            }
                                            return true;
                                        }

                                        // Quit URL check (exact or trailing slash match)
                                        if matches_quit_url(uri, &quit_url) {
                                            decision.ignore();
                                            act(PaneAction::QuitUrlTriggered { reset: reset_on_quit });
                                            return true;
                                        }

                                        // URL filter check (main frame) with HTML-escaped URI!
                                        if filter.evaluate(uri, ResourceType::MainFrame) == FilterDecision::Block {
                                            decision.ignore();
                                            let safe_uri = assets::escape_html(uri);
                                            let html = assets::BLOCKED_PAGE_HTML.replace("%%URL%%", &safe_uri);
                                            wv.load_html(&html, Some(uri));
                                            return true;
                                        }

                                        // Moodle .seb configuration resource URL check -> download through WebKitGTK cookies!
                                        if allow_reconfig && is_configuration_resource(uri, "", "") {
                                            decision.ignore();
                                            act(PaneAction::ReconfigureDownload {
                                                pane_id: id,
                                                url: uri.to_string(),
                                            });
                                            return true;
                                        }
                                    }
                                }
                            }
                        }
                        false
                    }
                    PolicyDecisionType::Response => {
                        if let Some(resp_dec) = decision.downcast_ref::<webkit2gtk::ResponsePolicyDecision>() {
                            if let Some(resp) = resp_dec.response() {
                                let uri = resp.uri().map(|s| s.to_string()).unwrap_or_default();
                                let mime = resp.mime_type().map(|s| s.to_string()).unwrap_or_default();
                                let suggested = resp
                                    .suggested_filename()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default();
                                // Detect Generic LMS user identifier header (X-LMS-USER-ID) for SEB Server
                                if let Some(headers) = resp.http_headers() {
                                    if let Some(uid) = headers.one("X-LMS-USER-ID") {
                                        let trimmed = uid.trim();
                                        if !trimmed.is_empty() {
                                            act(PaneAction::LmsUserIdentifierDetected(
                                                trimmed.to_string(),
                                            ));
                                        }
                                    }
                                }
                                if allow_reconfig && is_configuration_resource(&uri, &suggested, &mime) {
                                    // Download via WebKitGTK so browser session cookies (e.g. MoodleSession) are included!
                                    decision.download();
                                    act(PaneAction::Log(format!(
                                        "SEB configuration download detected: {uri} ({suggested} / {mime})"
                                    )));
                                    return true;
                                }
                                if mime.eq_ignore_ascii_case("application/pdf") {
                                    if !allow_pdf {
                                        decision.download();
                                        return true;
                                    }
                                    if !resp_dec.is_mime_type_supported() {
                                        decision.ignore();
                                        act(PaneAction::OpenPdfInline {
                                            pane_id: id,
                                            url: uri,
                                        });
                                        return true;
                                    }
                                }
                            }
                        }
                        false
                    }
                    PolicyDecisionType::NewWindowAction => {
                        if let Some(nav) = decision.downcast_ref::<webkit2gtk::NavigationPolicyDecision>() {
                            if let Some(action) = nav.navigation_action() {
                                if let Some(req) = action.request() {
                                    if let Some(uri_gstr) = req.uri() {
                                        let uri = uri_gstr.to_string();
                                        let cur_url = p_pol.current_url.borrow().clone();
                                        decision.ignore();
                                        handle_popup_request(
                                            wv,
                                            &cur_url,
                                            &uri,
                                            popup_policy,
                                            &filter,
                                            &quit_url,
                                            reset_on_quit,
                                            allow_reconfig,
                                            id,
                                            &act,
                                        );
                                        return true;
                                    }
                                }
                            }
                        }
                        false
                    }
                    _ => false,
                }
            });
        }

        // Popup `create` signal (also runs full filter + quit URL + popup policy!)
        {
            let filter = Rc::new(RequestFilter::new(&seb_filter::FilterConfig {
                process_main_requests: settings.browser.filter.process_main_requests,
                process_content_requests: settings.browser.filter.process_content_requests,
                rules: settings.browser.filter.rules.clone(),
            }));
            let quit_url = settings.browser.quit_url.trim().to_string();
            let reset_on_quit = settings.browser.reset_on_quit_url;
            let allow_reconfig = settings.security.allow_reconfiguration
                && settings.browser.allow_configuration_downloads;
            let popup_policy = settings.browser.popup_policy;
            let p = Rc::clone(&pane);
            let act = Rc::clone(&on_action);
            web_view.connect_create(move |wv, action| {
                let uri = action
                    .request()
                    .and_then(|r| r.uri())
                    .map(|u| u.to_string())
                    .unwrap_or_default();
                if !uri.is_empty() {
                    let cur_url = p.current_url.borrow().clone();
                    handle_popup_request(
                        wv,
                        &cur_url,
                        &uri,
                        popup_policy,
                        &filter,
                        &quit_url,
                        reset_on_quit,
                        allow_reconfig,
                        id,
                        &act,
                    );
                }
                None
            });
        }

        // window.close() support for sub-windows (ignored on main window)
        {
            let act = Rc::clone(&on_action);
            web_view.connect_close(move |_| {
                if !is_main {
                    act(PaneAction::Close(id));
                }
            });
        }

        // Printing enforcement (`window.print()` blocked when !allow_print; guarded by external modal when allowed)
        {
            let allow_print = settings.browser.allow_print;
            let win_c = parent_window.clone();
            web_view.connect_print(move |_, op| {
                if allow_print {
                    dialogs::with_external_modal(|| {
                        op.run_dialog(Some(&win_c));
                    });
                }
                true
            });
        }

        // Themed modal JS dialogs (alert / confirm / prompt / beforeunload) with keep_above!
        {
            let win_c = parent_window.clone();
            web_view.connect_script_dialog(move |_, dlg| {
                let msg = dlg.message().map(|s| s.to_string()).unwrap_or_default();
                match dlg.dialog_type() {
                    ScriptDialogType::Alert => {
                        dialogs::show_message(Some(&win_c), "Page Message", &msg, false);
                    }
                    ScriptDialogType::Confirm => {
                        let ok = dialogs::show_confirm(Some(&win_c), "Confirm", &msg);
                        dlg.confirm_set_confirmed(ok);
                    }
                    ScriptDialogType::BeforeUnloadConfirm => {
                        let ok = dialogs::show_confirm(
                            Some(&win_c),
                            "Leave Page",
                            if msg.is_empty() {
                                "Are you sure you want to leave this page?"
                            } else {
                                &msg
                            },
                        );
                        dlg.confirm_set_confirmed(ok);
                    }
                    ScriptDialogType::Prompt => {
                        let default_txt = dlg
                            .prompt_get_default_text()
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        if let Some(val) =
                            dialogs::prompt_text(Some(&win_c), "Prompt", &msg, &default_txt)
                        {
                            dlg.prompt_set_text(&val);
                        }
                    }
                    _ => {}
                }
                true
            });
        }

        // HTTP Basic / Proxy Authentication signal
        {
            extern "C" {
                fn webkit_authentication_request_authenticate(
                    request: *mut libc::c_void,
                    credential: *mut libc::c_void,
                );
            }
            let submit_cred = |req: &webkit2gtk::AuthenticationRequest, cred: &mut Credential| {
                use glib::translate::ToGlibPtrMut;
                unsafe {
                    webkit_authentication_request_authenticate(
                        req.as_ptr() as *mut libc::c_void,
                        cred.to_glib_none_mut().0 as *mut libc::c_void,
                    );
                }
            };
            let win_c = parent_window.clone();
            let proxy_cfg = settings.browser.proxy.clone();
            web_view.connect_authenticate(move |_, req| {
                let host = req.host().map(|s| s.to_string()).unwrap_or_default();
                let is_proxy = req.is_for_proxy();
                if is_proxy && proxy_cfg.policy == ProxyPolicy::Custom {
                    if let Some(matched) = proxy_cfg.proxies.iter().find(|p| {
                        p.requires_authentication
                            && (p.host.eq_ignore_ascii_case(&host)
                                || p.protocol == ProxyProtocol::Socks)
                    }) {
                        let mut cred = Credential::new(
                            &matched.username,
                            &matched.password,
                            CredentialPersistence::ForSession,
                        );
                        submit_cred(req, &mut cred);
                        return true;
                    }
                }
                let realm = req
                    .realm()
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(host);
                if let Some((user, pass)) =
                    dialogs::prompt_credentials(Some(&win_c), "Authentication Required", &realm)
                {
                    let mut cred = Credential::new(&user, &pass, CredentialPersistence::ForSession);
                    submit_cred(req, &mut cred);
                } else {
                    req.cancel();
                }
                true
            });
        }

        // Strict TLS/SSL certificate error rejection with clean light-themed page
        {
            web_view.connect_load_failed_with_tls_errors(move |wv, failing_uri, _, _| {
                let safe_uri = assets::escape_html(failing_uri);
                let html = assets::TLS_ERROR_HTML.replace("%%URL%%", &safe_uri);
                wv.load_html(&html, Some(failing_uri));
                true
            });
        }

        // Context menu policy
        {
            let allow_right = settings.mouse.allow_right_button;
            web_view.connect_context_menu(move |_, _, _, _| !allow_right);
        }

        // Media Capture (camera / microphone / screen share), geolocation, and notification permissions
        {
            let allow_cam = settings.browser.media_capture_camera;
            let allow_mic = settings.browser.media_capture_microphone;
            web_view.connect_permission_request(move |_, req| {
                if let Some(um) = req.downcast_ref::<UserMediaPermissionRequest>() {
                    let want_audio = um.is_for_audio_device();
                    let want_video = um.is_for_video_device();
                    let permitted = (want_audio || want_video)
                        && (!want_audio || allow_mic)
                        && (!want_video || allow_cam);
                    if permitted {
                        req.allow();
                    } else {
                        req.deny();
                    }
                    return true;
                }
                if req.is::<DeviceInfoPermissionRequest>() {
                    if allow_cam || allow_mic {
                        req.allow();
                    } else {
                        req.deny();
                    }
                    return true;
                }
                req.deny();
                true
            });
        }

        // Sandboxed File Upload Chooser (parity with browser_window.cpp:304-429!)
        {
            let allow_uploads = settings.browser.allow_uploads;
            let allow_custom = settings.browser.allow_custom_down_upload_location;
            let win_c = parent_window.clone();
            let base_dir = upload_base_dir;
            web_view.connect_run_file_chooser(move |_, req| {
                if !allow_uploads {
                    req.cancel();
                    return true;
                }
                let multiple = req.selects_multiple();
                let mimes: Vec<String> = req
                    .mime_types()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect();
                let selected = dialogs::show_upload_file_chooser(
                    Some(&win_c),
                    &base_dir,
                    multiple,
                    allow_custom,
                    &mimes,
                );
                if selected.is_empty() {
                    req.cancel();
                } else {
                    let strs: Vec<String> = selected
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    let refs: Vec<&str> = strs.iter().map(|s| s.as_str()).collect();
                    req.select_files(&refs);
                }
                true
            });
        }

        // Ctrl + Mouse Wheel zoom & blocking
        {
            let allow_zoom = settings.browser.allow_page_zoom;
            let p = Rc::clone(&pane);
            web_view.add_events(gdk::EventMask::SCROLL_MASK);
            web_view.connect_scroll_event(move |_, ev| {
                if ev.state().contains(gdk::ModifierType::CONTROL_MASK) {
                    if allow_zoom {
                        let (_, dy) = ev.delta();
                        if dy < 0.0 || ev.direction() == gdk::ScrollDirection::Up {
                            p.adjust_zoom_steps(1);
                        } else if dy > 0.0 || ev.direction() == gdk::ScrollDirection::Down {
                            p.adjust_zoom_steps(-1);
                        }
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }

        // Mouse button policy (Middle button, Mouse Back 8 / Forward 9) + Proctoring interaction
        {
            let allow_middle = settings.mouse.allow_middle_button
                && settings.security.clipboard_policy == ClipboardPolicy::Allow;
            let allow_back = window_settings.allow_backward_navigation;
            let allow_fwd = window_settings.allow_forward_navigation;
            let act = Rc::clone(&on_action);
            let p_click = Rc::clone(&pane);
            web_view.connect_button_press_event(move |wv, ev| {
                p_click.close_zoom_popup();
                let button = ev.button();
                if button == 2 && !allow_middle {
                    return glib::Propagation::Stop;
                }
                if button == 8 {
                    if allow_back {
                        wv.go_back();
                    }
                    return glib::Propagation::Stop;
                }
                if button == 9 {
                    if allow_fwd {
                        wv.go_forward();
                    }
                    return glib::Propagation::Stop;
                }
                let (x, y) = ev.position();
                let btn_name = match button {
                    3 => "Right",
                    2 => "Middle",
                    _ => "Left",
                };
                act(PaneAction::UserInteraction(format!(
                    "{btn_name} mouse button pressed at ({}/{}).",
                    x as i32, y as i32
                )));
                act(PaneAction::Focus(id));
                glib::Propagation::Proceed
            });
        }

        if !initial_url.is_empty() {
            web_view.load_uri(initial_url);
        }

        pane
    }

    fn wire_resize_handles(self: &Rc<Self>, workspace: &gtk::Layout, on_action: &Rc<dyn Fn(PaneAction)>) {
        #[derive(Default)]
        struct ResizeState {
            active: bool,
            edges: u8,
            start_rx: f64,
            start_ry: f64,
            orig: Option<PaneRect>,
        }
        let rstate = Rc::new(RefCell::new(ResizeState::default()));
        let base_edges = [RESIZE_LEFT, RESIZE_RIGHT, RESIZE_TOP, RESIZE_BOTTOM];

        for (idx, strip) in self.resize_strips.iter().enumerate() {
            let primary_edge = base_edges[idx];

            let compute_edges = {
                let p = Rc::clone(self);
                move |ex: f64, ey: f64| -> u8 {
                    let cur = *p.current_rect.borrow();
                    let mut mask = primary_edge;
                    if primary_edge == RESIZE_LEFT || primary_edge == RESIZE_RIGHT {
                        if ey <= (RESIZE_BORDER * 2) as f64 {
                            mask |= RESIZE_TOP;
                        } else if ey >= (cur.h - RESIZE_BORDER * 2) as f64 {
                            mask |= RESIZE_BOTTOM;
                        }
                    } else if ex <= (RESIZE_BORDER * 2) as f64 {
                        mask |= RESIZE_LEFT;
                    } else if ex >= (cur.w - RESIZE_BORDER * 2) as f64 {
                        mask |= RESIZE_RIGHT;
                    }
                    mask
                }
            };

            // Cursor hover update
            {
                let ce = compute_edges.clone();
                strip.connect_motion_notify_event(move |w, ev| {
                    let (ex, ey) = ev.position();
                    let mask = ce(ex, ey);
                    set_resize_cursor(w, mask);
                    glib::Propagation::Proceed
                });
            }
            {
                strip.connect_leave_notify_event(move |w, _| {
                    if let Some(win) = w.window() {
                        win.set_cursor(None);
                    }
                    glib::Propagation::Proceed
                });
            }

            // Button press starts resize
            {
                let rs = Rc::clone(&rstate);
                let p = Rc::clone(self);
                let ws_rp = workspace.clone();
                let act = Rc::clone(on_action);
                let ce = compute_edges.clone();
                strip.connect_button_press_event(move |w, ev| {
                    if ev.button() != 1 || p.maximized.get() {
                        return glib::Propagation::Proceed;
                    }
                    act(PaneAction::Focus(p.id));
                    let (ex, ey) = ev.position();
                    let (rx, ry, _) = workspace_pointer(&ws_rp, ev.device().as_ref(), ev.root());
                    let mask = ce(ex, ey);
                    set_resize_cursor(w, mask);
                    *rs.borrow_mut() = ResizeState {
                        active: true,
                        edges: mask,
                        start_rx: rx,
                        start_ry: ry,
                        orig: Some(*p.current_rect.borrow()),
                    };
                    glib::Propagation::Stop
                });
            }

            // Drag resizes pane clamped to workspace & min size
            {
                let rs = Rc::clone(&rstate);
                let p = Rc::clone(self);
                let ws = workspace.clone();
                strip.connect_motion_notify_event(move |w, ev| {
                    if !ev.state().contains(gdk::ModifierType::BUTTON1_MASK) {
                        let mut st = rs.borrow_mut();
                        if st.active {
                            st.active = false;
                            st.orig = None;
                            if let Some(win) = w.window() {
                                win.set_cursor(None);
                            }
                        }
                        return glib::Propagation::Proceed;
                    }
                    let (active, orig_opt, start_rx, start_ry, edges) = {
                        let st = rs.borrow();
                        (st.active, st.orig, st.start_rx, st.start_ry, st.edges)
                    };
                    if !active {
                        return glib::Propagation::Proceed;
                    }
                    let Some(orig) = orig_opt else {
                        return glib::Propagation::Proceed;
                    };
                    let (rx, ry, _) = workspace_pointer(&ws, ev.device().as_ref(), ev.root());
                    let dx = (rx - start_rx) as i32;
                    let dy = (ry - start_ry) as i32;
                    let ws_w = ws.allocated_width().max(MIN_WINDOW_WIDTH);
                    let ws_h = ws.allocated_height().max(MIN_WINDOW_HEIGHT);

                    let mut left = orig.x;
                    let mut right = orig.x + orig.w;
                    let mut top = orig.y;
                    let mut bottom = orig.y + orig.h;

                    let (min_w, min_h) = p.min_size();
                    if edges & RESIZE_LEFT != 0 {
                        left = (orig.x + dx).clamp(0, right - min_w);
                    }
                    if edges & RESIZE_RIGHT != 0 {
                        right = (orig.x + orig.w + dx).clamp(left + min_w, ws_w);
                    }
                    if edges & RESIZE_TOP != 0 {
                        top = (orig.y + dy).clamp(0, bottom - min_h);
                    }
                    if edges & RESIZE_BOTTOM != 0 {
                        bottom = (orig.y + orig.h + dy).clamp(top + min_h, ws_h);
                    }

                    p.set_windowed_rect(
                        &ws,
                        PaneRect {
                            x: left,
                            y: top,
                            w: right - left,
                            h: bottom - top,
                        },
                        false,
                    );
                    glib::Propagation::Stop
                });
            }

            // Release / grab-broken finishes resize
            {
                let rs = Rc::clone(&rstate);
                strip.connect_grab_broken_event(move |w, _| {
                    let mut st = rs.borrow_mut();
                    st.active = false;
                    st.orig = None;
                    if let Some(win) = w.window() {
                        win.set_cursor(None);
                    }
                    glib::Propagation::Proceed
                });
            }
            {
                let rs = Rc::clone(&rstate);
                strip.connect_button_release_event(move |w, _| {
                    let mut st = rs.borrow_mut();
                    if st.active {
                        st.active = false;
                        st.orig = None;
                        if let Some(win) = w.window() {
                            win.set_cursor(None);
                        }
                        return glib::Propagation::Stop;
                    }
                    glib::Propagation::Proceed
                });
            }
        }
    }

    fn update_resize_strip_visibility(&self) {
        let show = !self.maximized.get();
        for strip in &self.resize_strips {
            if show {
                strip.show();
            } else {
                strip.hide();
            }
        }
    }

    pub fn sync_navigation_chrome(&self) {
        if let Some(ref b) = self.back_btn {
            b.set_sensitive(self.web_view.can_go_back());
        }
        if let Some(ref b) = self.fwd_btn {
            b.set_sensitive(self.web_view.can_go_forward());
        }
        let url = self.current_url.borrow().clone();
        let title = self.current_title.borrow().clone();
        if let Some(ref e) = self.url_entry {
            e.set_text(&url);
        }
        if let Some(ref lbl) = self.toolbar_title_label {
            if self.window_settings.url_policy == UrlPolicy::BeforeTitle && !url.is_empty() {
                lbl.set_text(&format!("{url}  —  {title}"));
            } else {
                lbl.set_text(&title);
            }
        }
    }

    pub fn compute_initial_subwindow_rect(ws_w: i32, ws_h: i32, ws: &WindowSettings) -> PaneRect {
        let w = if let Some(aw) = ws.absolute_width {
            aw
        } else {
            let pct = ws.relative_width.unwrap_or(70).clamp(20, 100);
            (ws_w * pct) / 100
        }
        .clamp(MIN_WINDOW_WIDTH, ws_w.max(MIN_WINDOW_WIDTH));

        let h = if let Some(ah) = ws.absolute_height {
            ah
        } else {
            let pct = ws.relative_height.unwrap_or(80).clamp(20, 100);
            (ws_h * pct) / 100
        }
        .clamp(MIN_WINDOW_HEIGHT, ws_h.max(MIN_WINDOW_HEIGHT));

        let x = match ws.position {
            WindowPosition::Left => 0,
            WindowPosition::Center => ((ws_w - w) / 2).max(0),
            WindowPosition::Right => (ws_w - w).max(0),
        };
        let y = ((ws_h - h) / 2).max(0);
        PaneRect { x, y, w, h }
    }

    pub fn maximize(&self, workspace: &gtk::Layout) {
        if !self.maximized.get() && !self.snapped.get() {
            *self.restored_rect.borrow_mut() = *self.current_rect.borrow();
        }
        self.maximized.set(true);
        self.snapped.set(false);
        self.frame_box.style_context().remove_class("windowed");
        self.frame_box.style_context().remove_class("snapped");
        self.max_btn.set_tooltip_text(Some("Restore"));
        self.max_btn
            .set_image(Some(&assets::image_from_bytes(assets::restore_icon_svg(), 14, 14)));
        self.update_resize_strip_visibility();
        let raw_w = workspace.allocated_width();
        let raw_h = workspace.allocated_height();
        let w = raw_w.max(MIN_WINDOW_WIDTH);
        let h = raw_h.max(MIN_WINDOW_HEIGHT);
        if raw_w < 100 || raw_h < 100 {
            // Workspace not allocated yet: mark restore rect invalid so Restore recomputes it.
            *self.restored_rect.borrow_mut() = PaneRect { x: 0, y: 0, w: 0, h: 0 };
        } else {
            let mut rr = self.restored_rect.borrow_mut();
            if rr.w >= w || rr.w < MIN_WINDOW_WIDTH {
                let rw = (w * 80 / 100).max(480).min(w);
                let rh = (h * 80 / 100).max(360).min(h);
                *rr = PaneRect {
                    x: (w - rw) / 2,
                    y: (h - rh) / 2,
                    w: rw,
                    h: rh,
                };
            }
        }
        self.apply_rect(workspace, PaneRect { x: 0, y: 0, w, h });
    }

    pub fn restore(&self, workspace: &gtk::Layout) {
        self.maximized.set(false);
        self.snapped.set(false);
        self.frame_box.style_context().remove_class("snapped");
        self.frame_box.style_context().add_class("windowed");
        self.max_btn.set_tooltip_text(Some("Maximize"));
        self.max_btn
            .set_image(Some(&assets::image_from_bytes(assets::maximize_icon_svg(), 14, 14)));
        self.update_resize_strip_visibility();
        let ws_w = workspace.allocated_width().max(MIN_WINDOW_WIDTH);
        let ws_h = workspace.allocated_height().max(MIN_WINDOW_HEIGHT);
        let mut r = *self.restored_rect.borrow();
        if r.w >= ws_w || r.w < MIN_WINDOW_WIDTH {
            r.w = (ws_w * 80 / 100).max(480).min(ws_w);
            r.h = (ws_h * 80 / 100).max(360).min(ws_h);
            r.x = (ws_w - r.w) / 2;
            r.y = (ws_h - r.h) / 2;
        } else {
            r.w = r.w.clamp(MIN_WINDOW_WIDTH, ws_w);
            r.h = r.h.clamp(MIN_WINDOW_HEIGHT, ws_h);
            r.x = r.x.clamp(-r.w + 80, (ws_w - 80).max(0));
            r.y = r.y.clamp(0, (ws_h - TITLE_BAR_HEIGHT).max(0));
        }
        self.apply_rect(workspace, r);
    }

    pub fn toggle_maximize(&self, workspace: &gtk::Layout) {
        if self.maximized.get() {
            self.restore(workspace);
        } else {
            self.maximize(workspace);
        }
    }

    pub fn set_windowed_rect(&self, workspace: &gtk::Layout, rect: PaneRect, snapped: bool) {
        self.maximized.set(false);
        self.snapped.set(snapped);
        let ctx = self.frame_box.style_context();
        if snapped {
            ctx.remove_class("windowed");
            ctx.add_class("snapped");
        } else {
            ctx.remove_class("snapped");
            ctx.add_class("windowed");
            *self.restored_rect.borrow_mut() = rect;
        }
        self.max_btn.set_tooltip_text(Some("Maximize"));
        self.max_btn
            .set_image(Some(&assets::image_from_bytes(assets::maximize_icon_svg(), 14, 14)));
        self.update_resize_strip_visibility();
        self.apply_rect(workspace, rect);
    }

    pub fn apply_rect(&self, workspace: &gtk::Layout, rect: PaneRect) {
        let prev = *self.current_rect.borrow();
        *self.current_rect.borrow_mut() = rect;
        if self.last_requested_size.get() != (rect.w, rect.h) {
            self.last_requested_size.set((rect.w, rect.h));
            self.root.set_size_request(rect.w, rect.h);
        }
        if prev.x != rect.x || prev.y != rect.y {
            workspace.move_(&self.root, rect.x, rect.y);
        }
    }

    pub fn on_workspace_resized(&self, workspace: &gtk::Layout, alloc_w: i32, alloc_h: i32) {
        let ws_w = alloc_w.max(MIN_WINDOW_WIDTH);
        let ws_h = alloc_h.max(MIN_WINDOW_HEIGHT);
        let (min_w, min_h) = self.min_size();
        let target = if self.maximized.get() {
            PaneRect { x: 0, y: 0, w: ws_w, h: ws_h }
        } else {
            let mut cur = *self.current_rect.borrow();
            cur.w = cur.w.clamp(min_w, ws_w);
            cur.h = cur.h.clamp(min_h, ws_h);
            cur.x = cur.x.clamp(-cur.w + 80, (ws_w - 80).max(0));
            cur.y = cur.y.clamp(0, (ws_h - TITLE_BAR_HEIGHT).max(0));
            cur
        };
        self.apply_rect(workspace, target);
        // gtk::Layout allocates children using their requisition inside Layout::size_allocate,
        // so after updating root.set_size_request(...) mid-allocation we must queue BOTH
        // root and workspace once so gtk::Layout assigns the updated rect (guarded by last_ws_size).
        let ws = workspace.clone();
        let root = self.root.clone();
        glib::idle_add_local_once(move || {
            root.queue_resize();
            ws.queue_resize();
        });
    }

    pub fn set_minimized(&self, minimized: bool) {
        if minimized {
            // Preserve existing live WebView snapshot if already cached; only fallback to root draw if empty
            if self.last_preview.borrow().is_none() {
                let _ = self.preview_pixbuf(202, 112);
            }
            self.minimized.set(true);
            self.root.hide();
        } else {
            self.minimized.set(false);
            self.root.show();
        }
    }

    /// 5% zoom steps clamped to 25%–300% (100% parity with C++ browser_window.cpp!)
    pub fn adjust_zoom_steps(&self, steps: i32) {
        let next = (self.zoom_percent.get() + steps * 5).clamp(25, 300);
        self.set_zoom_percent(next);
    }

    pub fn set_zoom_percent(&self, pct: i32) {
        let clamped = pct.clamp(25, 300);
        self.zoom_percent.set(clamped);
        self.web_view.set_zoom_level(clamped as f64 / 100.0);
        if let Some(ref btn) = *self.zoom_pct_label.borrow() {
            btn.set_label(&format!("{clamped}%"));
        }
    }

    pub fn reset_zoom(&self) {
        self.set_zoom_percent(100);
    }

    fn toggle_zoom_popup(&self) {
        if self.zoom_overlay.is_visible() {
            self.zoom_overlay.hide();
        } else {
            self.close_find_bar();
            if let Some(ref lbl) = *self.zoom_pct_label.borrow() {
                lbl.set_label(&format!("{}%", self.zoom_percent.get()));
            }
            self.zoom_overlay.show();
            self.overlay.queue_resize();
        }
    }

    fn close_zoom_popup(&self) {
        if self.zoom_overlay.is_visible() {
            self.zoom_overlay.hide();
        }
    }

    pub fn toggle_find_bar(&self) {
        if self.find_overlay.is_visible() {
            self.close_find_bar();
        } else {
            self.close_zoom_popup();
            self.find_overlay.show();
            self.overlay.queue_resize();
            self.find_entry.grab_focus();
            self.find_entry.select_region(0, -1);
        }
    }

    pub fn close_find_bar(&self) {
        if self.find_overlay.is_visible() {
            self.find_overlay.hide();
        }
        if let Some(fc) = self.web_view.find_controller() {
            fc.search_finish();
        }
    }

    pub fn is_find_bar_visible(&self) -> bool {
        self.find_overlay.is_visible()
    }

    pub fn focus_address_bar(&self) {
        if self.window_settings.allow_address_bar {
            if let Some(ref e) = self.url_entry {
                e.grab_focus();
                e.select_region(0, -1);
            }
        }
    }

    pub fn reload(&self) {
        if !self.window_settings.allow_reloading || self.reload_confirm_active.get() {
            return;
        }
        if self.window_settings.show_reload_warning {
            self.reload_confirm_active.set(true);
            let ok = dialogs::show_confirm(
                None,
                "Reload Page",
                "Are you sure you want to reload the current page?",
            );
            self.reload_confirm_active.set(false);
            if !ok {
                return;
            }
        }
        self.web_view.reload();
    }

    pub fn navigate_home(&self) {
        if self.home_url_target.trim().is_empty() {
            return;
        }
        if !self.home_message.trim().is_empty()
            && !dialogs::show_confirm(None, "Navigate Home", &self.home_message)
        {
            return;
        }
        if self.home_requires_password && !self.home_password_hash.is_empty() {
            loop {
                let Some(entered) = dialogs::prompt_password(
                    None,
                    "Navigate Home",
                    "Enter the password to navigate back to the start URL:",
                ) else {
                    return;
                };
                if verify_password_or_hash(&entered, &self.home_password_hash) {
                    break;
                }
                dialogs::show_message(None, "Invalid Password", "Incorrect password.", true);
            }
        }
        self.web_view.load_uri(&self.home_url_target);
    }

    pub fn open_dev_tools(&self) {
        if self.window_settings.allow_developer_console {
            if let Some(insp) = self.web_view.inspector() {
                insp.show();
            }
        }
    }

    fn min_size(&self) -> (i32, i32) {
        if self.floating_proctoring.get() {
            (160, 120)
        } else {
            (MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)
        }
    }

    pub fn print_page(&self) {
        if self.allow_print {
            let parent = self
                .root
                .toplevel()
                .and_then(|w| w.downcast::<gtk::Window>().ok());
            let op = PrintOperation::new(&self.web_view);
            dialogs::with_external_modal(|| {
                op.run_dialog(parent.as_ref());
            });
        }
    }

    /// Converts this pane into a borderless, always-on-top floating proctoring window:
    /// hides titlebar & toolbar, allows shrinking to 160x120, and lets the user drag it freely.
    pub fn enable_floating_proctoring_mode(self: &Rc<Self>, workspace: &gtk::Layout) {
        self.floating_proctoring.set(true);
        self.title_event_box.set_no_show_all(true);
        self.title_event_box.hide();
        // Hide any top hairline divider above the webview
        for child in self.frame_box.children() {
            if child.style_context().has_class("seb-hairline")
                || child.style_context().has_class("seb-toolbar")
            {
                child.set_no_show_all(true);
                child.hide();
            }
        }

        #[derive(Default)]
        struct FloatDrag {
            active: bool,
            dragging: bool,
            start_rx: f64,
            start_ry: f64,
            orig_x: i32,
            orig_y: i32,
        }
        let drag = Rc::new(RefCell::new(FloatDrag::default()));
        {
            let d = Rc::clone(&drag);
            let p = Rc::clone(self);
            self.web_view.connect_button_press_event(move |_, ev| {
                if ev.button() == 1 {
                    let (rx, ry) = ev.root();
                    let cur = *p.current_rect.borrow();
                    let mut st = d.borrow_mut();
                    st.active = true;
                    st.dragging = false;
                    st.start_rx = rx;
                    st.start_ry = ry;
                    st.orig_x = cur.x;
                    st.orig_y = cur.y;
                }
                glib::Propagation::Proceed
            });
        }
        {
            let d = Rc::clone(&drag);
            let p = Rc::clone(self);
            let ws = workspace.clone();
            self.web_view.connect_motion_notify_event(move |_, ev| {
                if !ev.state().contains(gdk::ModifierType::BUTTON1_MASK) {
                    let mut st = d.borrow_mut();
                    st.active = false;
                    st.dragging = false;
                    return glib::Propagation::Proceed;
                }
                let mut st = d.borrow_mut();
                if !st.active {
                    return glib::Propagation::Proceed;
                }
                let (rx, ry) = ev.root();
                if !st.dragging
                    && (rx - st.start_rx).abs().max((ry - st.start_ry).abs()) >= 4.0
                {
                    st.dragging = true;
                }
                let is_dragging = st.dragging;
                let orig_x = st.orig_x;
                let orig_y = st.orig_y;
                let start_rx = st.start_rx;
                let start_ry = st.start_ry;
                drop(st);
                if is_dragging {
                    let ws_w = ws.allocated_width().max(MIN_WINDOW_WIDTH);
                    let ws_h = ws.allocated_height().max(MIN_WINDOW_HEIGHT);
                    let cur = *p.current_rect.borrow();
                    let nx = (orig_x + (rx - start_rx) as i32).clamp(0, (ws_w - cur.w).max(0));
                    let ny = (orig_y + (ry - start_ry) as i32).clamp(0, (ws_h - cur.h).max(0));
                    p.set_windowed_rect(
                        &ws,
                        PaneRect {
                            x: nx,
                            y: ny,
                            w: cur.w,
                            h: cur.h,
                        },
                        false,
                    );
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        {
            let d = Rc::clone(&drag);
            self.web_view.connect_grab_broken_event(move |_, _| {
                let mut st = d.borrow_mut();
                st.active = false;
                st.dragging = false;
                glib::Propagation::Proceed
            });
        }
        {
            let d = Rc::clone(&drag);
            self.web_view.connect_button_release_event(move |_, _| {
                let mut st = d.borrow_mut();
                let was_dragging = st.dragging;
                st.active = false;
                st.dragging = false;
                if was_dragging {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
    }

    pub fn go_back(&self) {
        if self.window_settings.allow_backward_navigation {
            self.web_view.go_back();
        }
    }

    pub fn go_forward(&self) {
        if self.window_settings.allow_forward_navigation {
            self.web_view.go_forward();
        }
    }

    pub fn sync_isolated_clipboard(&self, id: &str, b64: &str) {
        let script = format!(
            "window.__sebClipboard && window.__sebClipboard.update({}, {});",
            serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into()),
            serde_json::to_string(b64).unwrap_or_else(|_| "\"\"".into()),
        );
        self.web_view
            .run_javascript(&script, gio::Cancellable::NONE, |_| {});
    }

    pub fn exec_isolated_clipboard(&self, cmd: &str) {
        let script = format!(
            "window.__sebClipboard && window.__sebClipboard.exec({});",
            serde_json::to_string(cmd).unwrap_or_else(|_| "\"\"".into()),
        );
        self.web_view
            .run_javascript(&script, gio::Cancellable::NONE, |_| {});
    }

    pub fn title(&self) -> String {
        self.current_title.borrow().clone()
    }

    pub fn url(&self) -> String {
        self.current_url.borrow().clone()
    }

    pub fn favicon_pixbuf(&self, size: i32) -> Option<gdk_pixbuf::Pixbuf> {
        let sz = size.max(8);
        self.current_favicon
            .borrow()
            .as_ref()
            .and_then(|pb| pb.scale_simple(sz, sz, gdk_pixbuf::InterpType::Bilinear))
    }

    /// Returns cached live WebView preview if available, or captures a fallback Cairo draw.
    pub fn preview_pixbuf(&self, target_w: i32, target_h: i32) -> Option<gdk_pixbuf::Pixbuf> {
        if let Some(cached) = self.last_preview.borrow().clone() {
            return cached
                .scale_simple(
                    target_w.max(16),
                    target_h.max(16),
                    gdk_pixbuf::InterpType::Bilinear,
                )
                .or(Some(cached));
        }
        if self.minimized.get() || !self.root.is_visible() || !self.root.is_mapped() {
            return None;
        }
        let alloc = self.root.allocation();
        let w = alloc.width();
        let h = alloc.height();
        if w <= 0 || h <= 0 {
            return None;
        }
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
        let cr = cairo::Context::new(&surface).ok()?;
        self.root.draw(&cr);
        drop(cr);
        let pb = gdk::pixbuf_get_from_surface(&surface, 0, 0, w, h)?;
        let scaled = pb.scale_simple(
            target_w.max(16),
            target_h.max(16),
            gdk_pixbuf::InterpType::Bilinear,
        );
        if scaled.is_some() {
            *self.last_preview.borrow_mut() = scaled.clone();
        }
        scaled
    }

    /// Asynchronously captures a real accelerated WebKitGTK visible snapshot (works on Wayland
    /// & X11 DMABuf!) and updates `last_preview` + invokes `on_ready`.
    pub fn refresh_preview_async(
        self: &Rc<Self>,
        target_w: i32,
        target_h: i32,
        on_ready: Option<Box<dyn Fn(gdk_pixbuf::Pixbuf)>>,
    ) {
        if self.minimized.get() || !self.root.is_visible() || !self.web_view.is_mapped() {
            return;
        }
        let p = Rc::clone(self);
        let tw = target_w.max(16);
        let th = target_h.max(16);
        self.web_view.snapshot(
            webkit2gtk::SnapshotRegion::Visible,
            webkit2gtk::SnapshotOptions::NONE,
            gio::Cancellable::NONE,
            move |res| {
                if let Ok(surf) = res {
                    if let Some(pb) = surface_to_pixbuf(surf) {
                        let scaled = pb
                            .scale_simple(tw, th, gdk_pixbuf::InterpType::Bilinear)
                            .unwrap_or(pb);
                        *p.last_preview.borrow_mut() = Some(scaled.clone());
                        if let Some(ref cb) = on_ready {
                            cb(scaled);
                        }
                    }
                }
            },
        );
    }
}

fn surface_to_pixbuf(surface: cairo::Surface) -> Option<gdk_pixbuf::Pixbuf> {
    let img = cairo::ImageSurface::try_from(surface).ok()?;
    let w = img.width();
    let h = img.height();
    if w <= 0 || h <= 0 {
        return None;
    }
    gdk::pixbuf_get_from_surface(&img, 0, 0, w, h)
}

pub fn verify_password_or_hash(entered: &str, expected_hash: &str) -> bool {
    if expected_hash.is_empty() {
        return true;
    }
    if expected_hash.len() == 64 && expected_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        seb_config::sha256_hex(entered.as_bytes()).eq_ignore_ascii_case(expected_hash)
    } else {
        entered == expected_hash
    }
}

fn normalize_entered_url(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("sebs://") {
        format!("https://{}", &raw[7..])
    } else if lower.starts_with("seb://") {
        format!("http://{}", &raw[6..])
    } else if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    }
}

fn matches_quit_url(uri: &str, quit_url: &str) -> bool {
    let q = quit_url.trim().trim_end_matches('/');
    if q.is_empty() {
        return false;
    }
    let u = uri.trim().trim_end_matches('/');
    u.eq_ignore_ascii_case(q)
}

#[allow(clippy::too_many_arguments)]
fn handle_popup_request(
    wv: &WebView,
    current_url: &str,
    uri: &str,
    popup_policy: PopupPolicy,
    filter: &RequestFilter,
    quit_url: &str,
    reset_on_quit: bool,
    allow_reconfig: bool,
    pane_id: u64,
    act: &Rc<dyn Fn(PaneAction)>,
) {
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("file://") || lower.starts_with("mailto:") {
        return;
    }
    if lower.starts_with("seb://") || lower.starts_with("sebs://") {
        if allow_reconfig {
            act(PaneAction::ReconfigureDownload {
                pane_id,
                url: uri.to_string(),
            });
        }
        return;
    }
    if matches_quit_url(uri, quit_url) {
        act(PaneAction::QuitUrlTriggered {
            reset: reset_on_quit,
        });
        return;
    }
    if filter.evaluate(uri, ResourceType::MainFrame) == FilterDecision::Block {
        return;
    }
    let same_host = match (Url::parse(current_url), Url::parse(uri)) {
        (Ok(a), Ok(b)) => match (a.host_str(), b.host_str()) {
            (Some(ha), Some(hb)) => ha.eq_ignore_ascii_case(hb),
            _ => false,
        },
        _ => false,
    };
    match popup_policy {
        PopupPolicy::Block => {}
        PopupPolicy::AllowSameWindow => wv.load_uri(uri),
        PopupPolicy::AllowSameHostAndWindow => {
            if same_host {
                wv.load_uri(uri);
            }
        }
        PopupPolicy::AllowSameHost => {
            if same_host {
                act(PaneAction::OpenPopup {
                    url: uri.to_string(),
                });
            }
        }
        PopupPolicy::Allow => {
            act(PaneAction::OpenPopup {
                url: uri.to_string(),
            });
        }
    }
}

fn set_resize_cursor(widget: &gtk::EventBox, mask: u8) {
    let Some(win) = widget.window() else { return };
    let name = match (
        mask & RESIZE_LEFT != 0,
        mask & RESIZE_RIGHT != 0,
        mask & RESIZE_TOP != 0,
        mask & RESIZE_BOTTOM != 0,
    ) {
        (true, _, true, _) | (_, true, _, true) => "nwse-resize",
        (_, true, true, _) | (true, _, _, true) => "nesw-resize",
        (true, _, _, _) | (_, true, _, _) => "ew-resize",
        (_, _, true, _) | (_, _, _, true) => "ns-resize",
        _ => return,
    };
    if let Some(display) = gdk::Display::default() {
        let cursor = gdk::Cursor::from_name(&display, name);
        win.set_cursor(cursor.as_ref());
    }
}

fn detect_snap_zone(x: i32, y: i32, ws_w: i32, ws_h: i32) -> SnapZone {
    let near_left = x <= SNAP_EDGE_THRESHOLD;
    let near_right = x >= ws_w - SNAP_EDGE_THRESHOLD;
    let near_top = y <= SNAP_EDGE_THRESHOLD;
    let near_bottom = y >= ws_h - SNAP_EDGE_THRESHOLD;

    if near_left && y <= SNAP_CORNER_THRESHOLD {
        return SnapZone::TopLeft;
    }
    if near_right && y <= SNAP_CORNER_THRESHOLD {
        return SnapZone::TopRight;
    }
    if near_left && y >= ws_h - SNAP_CORNER_THRESHOLD {
        return SnapZone::BottomLeft;
    }
    if near_right && y >= ws_h - SNAP_CORNER_THRESHOLD {
        return SnapZone::BottomRight;
    }
    if near_top {
        return SnapZone::Maximize;
    }
    if near_left {
        return SnapZone::Left;
    }
    if near_right {
        return SnapZone::Right;
    }
    if near_bottom {
        return SnapZone::None;
    }
    SnapZone::None
}

fn snap_rect_for_zone(zone: SnapZone, ws_w: i32, ws_h: i32) -> Option<PaneRect> {
    let half_w = ws_w / 2;
    let half_h = ws_h / 2;
    match zone {
        SnapZone::None => None,
        SnapZone::Maximize => Some(PaneRect {
            x: 0,
            y: 0,
            w: ws_w,
            h: ws_h,
        }),
        SnapZone::Left => Some(PaneRect {
            x: 0,
            y: 0,
            w: half_w,
            h: ws_h,
        }),
        SnapZone::Right => Some(PaneRect {
            x: half_w,
            y: 0,
            w: ws_w - half_w,
            h: ws_h,
        }),
        SnapZone::TopLeft => Some(PaneRect {
            x: 0,
            y: 0,
            w: half_w,
            h: half_h,
        }),
        SnapZone::TopRight => Some(PaneRect {
            x: half_w,
            y: 0,
            w: ws_w - half_w,
            h: half_h,
        }),
        SnapZone::BottomLeft => Some(PaneRect {
            x: 0,
            y: half_h,
            w: half_w,
            h: ws_h - half_h,
        }),
        SnapZone::BottomRight => Some(PaneRect {
            x: half_w,
            y: half_h,
            w: ws_w - half_w,
            h: ws_h - half_h,
        }),
    }
}

fn workspace_pointer(
    ws: &gtk::Layout,
    dev: Option<&gdk::Device>,
    fallback: (f64, f64),
) -> (f64, f64, Option<gdk::ModifierType>) {
    if let (Some(top), Some(d)) = (ws.toplevel(), dev) {
        if let Some(top_win) = top.window() {
            let (_, wx, wy, mask) = top_win.device_position_double(d);
            let (ox, oy) = ws.translate_coordinates(&top, 0, 0).unwrap_or((0, 0));
            return (wx - ox as f64, wy - oy as f64, Some(mask));
        }
    }
    (fallback.0, fallback.1, None)
}
