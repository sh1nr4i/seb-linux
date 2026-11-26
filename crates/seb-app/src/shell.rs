use crate::applications::ApplicationManager;
use crate::assets;
use crate::browser_pane::{verify_password_or_hash, BrowserPane, PaneAction, PaneRect};
use crate::clipboard::{clear_gtk_clipboards, IsolatedClipboard};
use crate::dialogs::{self, PreflightAction, RuntimeSplash, ServerFailureChoice};
use crate::overlays::{ActionCenterOverlay, ActionCenterTileAction, WindowSwitcherOverlay};
use crate::taskbar::{SebTaskbar, TaskbarAction};
use base64::Engine;
use gtk::prelude::*;
use seb_config::loader::{
    apply_browser_resource_profile, apply_start_url_query_from_resource, load_from_resource,
    process_settings_default, save_client_configuration, validate_proxy_settings, LoadStatus,
    LoadedConfiguration,
};
use seb_config::settings::{
    AppSettings, ClipboardPolicy, ConfigurationMode, KioskMode, ProctoringImageFormat,
    ProctoringImageQuantization, ProxyPolicy, ProxyProtocol, ServicePolicy, SessionConfiguration,
    SessionMode, UserInterfaceMode, VirtualMachinePolicy, SEB_APPLICATION_VERSION,
    SEB_COMPATIBILITY_BUILD_VERSION, SEB_COMPATIBILITY_VERSION,
};
use seb_config::{compute_browser_exam_key, sha256_hex};
use seb_filter::{download_url, is_configuration_resource};
use seb_platform::PlatformMonitor;
use seb_proctoring::client::{ProctoringClientConfig, ProctoringState, ScreenProctoringClient};
use seb_proctoring::{ImageFormatKind, ImageQuantizationKind, MetadataInput};
use seb_security::lockdown_client::{
    merge_prohibited_list, KeyboardLockPolicy, LockdownClient, LockdownEvent,
};
use seb_security::power::PowerInhibitor;
use seb_security::{
    detect_prohibited_processes, is_debugger_attached, is_virtual_machine, validate_display,
    validate_version_restrictions, SessionIntegrityStore,
};
use seb_server::client::{PingOutcome, SebServerClient, ServerConnectionConfig};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;
use webkit2gtk::{
    CookieAcceptPolicy, CookieManagerExt, CookiePersistentStorage, DownloadExt, NetworkProxyMode,
    NetworkProxySettings, URIRequestExt, URIResponseExt, WebContext, WebContextExt, WebViewExt,
    WebsiteDataManager, WebsiteDataManagerExt, WebsiteDataManagerExtManual, WebsiteDataTypes,
};

pub struct LaunchOptions {
    pub resource: Option<String>,
    pub dev_bypass: bool,
    pub strict_lockdown: bool,
}

type ArcServer = Arc<Mutex<SebServerClient>>;

#[derive(Default)]
struct PendingServerEvents {
    logs: VecDeque<(String, String)>,
    confirmations: VecDeque<String>,
    user_identifier: Option<String>,
    battery: Option<(i32, bool)>,
    wireless: Option<Option<i32>>,
}

fn drain_pending_server_events(srv: &ArcServer, queue: &Arc<Mutex<PendingServerEvents>>) {
    let drained = {
        let Ok(mut q) = queue.lock() else {
            return;
        };
        if q.logs.is_empty()
            && q.confirmations.is_empty()
            && q.user_identifier.is_none()
            && q.battery.is_none()
            && q.wireless.is_none()
        {
            return;
        }
        std::mem::take(&mut *q)
    };
    if let Ok(mut guard) = srv.lock() {
        for token in drained.confirmations {
            guard.enqueue_confirmation(&token);
        }
        if let Some(ref uid) = drained.user_identifier {
            let _ = guard.send_user_identifier(uid);
        }
        if let Some((pct, connected)) = drained.battery {
            guard.report_battery(pct, connected);
        }
        if let Some(sig) = drained.wireless {
            guard.report_wireless(sig);
        }
        for (msg, sev) in drained.logs {
            guard.log_event(&msg, &sev);
        }
    }
}

pub struct SebAppShell {
    window: gtk::Window,
    outer_box: gtk::Box,
    window_overlay: gtk::Overlay,
    workspace: gtk::Layout,
    snap_preview: gtk::Box,
    snap_preview_rect: Rc<Cell<Option<PaneRect>>>,
    lock_screen: gtk::Box,
    config: RefCell<SessionConfiguration>,
    current_admin_password_hash: RefCell<String>,
    panes: RefCell<Vec<Rc<BrowserPane>>>,
    pane_mru_order: RefCell<Vec<u64>>,
    next_pane_id: Cell<u64>,
    active_pane_id: Cell<u64>,
    taskbar: RefCell<Option<Rc<SebTaskbar>>>,
    action_center: RefCell<Option<Rc<ActionCenterOverlay>>>,
    switcher: Rc<WindowSwitcherOverlay>,
    switcher_origin_pane_id: Cell<Option<u64>>,
    switcher_peeked_minimized_id: Cell<Option<u64>>,
    switcher_peek_gen: Cell<u64>,
    platform: Rc<PlatformMonitor>,
    clipboard: Rc<IsolatedClipboard>,
    app_manager: Rc<RefCell<ApplicationManager>>,
    web_context: RefCell<Option<WebContext>>,
    active_downloads: RefCell<Vec<webkit2gtk::Download>>,
    temp_download_dir: RefCell<Option<PathBuf>>,
    temp_config_download_dir: RefCell<Option<PathBuf>>,
    temp_web_profile_dir: RefCell<Option<PathBuf>>,
    lockdown: RefCell<LockdownClient>,
    power_inhibitor: RefCell<PowerInhibitor>,
    integrity_store: RefCell<SessionIntegrityStore>,
    proctoring: RefCell<Option<Rc<ScreenProctoringClient>>>,
    last_proctoring_capture: RefCell<Instant>,
    pending_proctoring_action: RefCell<Option<String>>,
    deferred_proctoring_scheduled: Cell<bool>,
    server_client: RefCell<Option<ArcServer>>,
    server_rx: RefCell<Option<mpsc::Receiver<PingOutcome>>>,
    pending_server_events: Arc<Mutex<PendingServerEvents>>,
    server_wake_tx: RefCell<Option<mpsc::Sender<()>>>,
    server_ping_stop: RefCell<Option<Arc<AtomicBool>>>,
    last_detected_user_id: RefCell<String>,
    last_moodle_cookie_checked: RefCell<String>,
    live_proctoring_pane_id: Cell<Option<u64>>,
    last_battery_reported: Cell<Option<(i32, bool)>>,
    logs: Rc<RefCell<Vec<String>>>,
    strict_lockdown: Cell<bool>,
    dev_bypass: Cell<bool>,
    shutting_down: Cell<bool>,
    violation_dialog_active: Cell<bool>,
    reconfig_in_progress: Cell<bool>,
    quit_prompt_active: Cell<bool>,
    hand_raised: Cell<bool>,
    last_sidebar_toggle: RefCell<Instant>,
    last_focus_loss_warn: RefCell<Instant>,
    focus_restore_pending: Cell<bool>,
}

impl SebAppShell {
    pub fn launch(opts: LaunchOptions) -> Result<Rc<Self>, String> {
        let splash = RuntimeSplash::show(SEB_APPLICATION_VERSION, "Starting Safe Exam Browser...");

        let mut session_cfg = SessionConfiguration::default_session();
        process_settings_default(&mut session_cfg.settings);

        let initial_logs = vec![
            format!("Linux application version: \"{SEB_APPLICATION_VERSION}\""),
            format!("SEB compatibility version: \"{SEB_COMPATIBILITY_VERSION}\""),
        ];

        // 1. Load existing persistent client.seb first (for currentAdminPasswordHash & default fallback)
        let persistent_path = find_persistent_client_path(&session_cfg.app_config);
        let mut current_admin_hash = String::new();
        let mut persistent_loaded: Option<LoadedConfiguration> = None;

        if let Some(ref ppath) = persistent_path {
            if let Ok(LoadOutcome::Loaded(loaded)) = load_resource_with_password_sequence(
                &ppath.to_string_lossy(),
                "",
                opts.resource.is_none(),
            ) {
                current_admin_hash = loaded.settings.security.admin_password_hash.clone();
                persistent_loaded = Some(loaded);
            }
        }

        // 2. Load CLI resource or persistent client config
        if let Some(ref res) = opts.resource {
            splash.set_status("Loading SEB configuration...");
            match load_resource_with_password_sequence(res, &current_admin_hash, true) {
                Ok(LoadOutcome::Loaded(loaded)) => {
                    if loaded.settings.configuration_mode == ConfigurationMode::ConfigureClient {
                        splash.close();
                        verify_admin_password_for_client_config(
                            None,
                            &current_admin_hash,
                            &loaded,
                        )?;
                        save_client_configuration(&loaded, &session_cfg.app_config.app_data_file_path)?;
                        current_admin_hash = loaded.settings.security.admin_password_hash.clone();
                        let cont = dialogs::show_confirm(
                            None,
                            "Client Configuration Saved",
                            "The client configuration was saved successfully.\n\nContinue starting Safe Exam Browser?",
                        );
                        if !cont {
                            return Err("Client configuration saved successfully.".to_string());
                        }
                        session_cfg.settings = loaded.settings;
                        session_cfg.raw_seb_data = loaded.raw_seb_data;
                        session_cfg.is_client_configuration = true;
                    } else {
                        session_cfg.settings = loaded.settings;
                        session_cfg.raw_seb_data = loaded.raw_seb_data;
                        session_cfg.is_client_configuration = false;
                    }
                }
                Ok(LoadOutcome::OpenInBrowser(login_url)) => {
                    apply_browser_resource_profile(&mut session_cfg, &login_url);
                }
                Err(e) => {
                    splash.close();
                    dialogs::show_message(None, "Configuration Error", &e, true);
                    return Err(e);
                }
            }
        } else if let Some(loaded) = persistent_loaded {
            session_cfg.settings = loaded.settings;
            session_cfg.raw_seb_data = loaded.raw_seb_data;
            session_cfg.is_client_configuration = true;
        } else {
            session_cfg.settings.user_interface.taskbar.show_network = true;
            session_cfg.settings.user_interface.action_center.show_network = true;
        }

        if !session_cfg.settings.browser.enable_browser {
            splash.close();
            let msg = "This SEB configuration disables the browser (enableSebBrowser=false).";
            dialogs::show_message(None, "Unsupported Configuration", msg, true);
            return Err(msg.to_string());
        }

        // 3. SEB Server workflow if session_mode == Server
        let mut active_server: Option<ArcServer> = None;
        if session_cfg.settings.session_mode == SessionMode::Server
            && !session_cfg.settings.server.server_url.trim().is_empty()
        {
            splash.set_status("Connecting to SEB Server...");
            match run_server_workflow(&mut session_cfg, &splash, &current_admin_hash) {
                Ok(srv) => active_server = srv,
                Err(e) => {
                    splash.close();
                    dialogs::show_message(None, "SEB Server Error", &e, true);
                    return Err(e);
                }
            }
        }

        // 4. Security & Proxy validations
        splash.set_status("Validating security and environment...");
        if let Err(e) = validate_proxy_settings(&session_cfg.settings.browser.proxy) {
            splash.close();
            dialogs::show_message(None, "Invalid Proxy Configuration", &e, true);
            return Err(e);
        }

        let mut integrity_store =
            SessionIntegrityStore::new(&session_cfg.app_config.session_cache_file_path);
        let mut power_inhibitor = PowerInhibitor::new();
        if !opts.dev_bypass {
            if let Err(e) = validate_runtime_security(
                &session_cfg,
                &mut integrity_store,
                opts.strict_lockdown,
            ) {
                splash.close();
                dialogs::show_message(None, "Security Check Failed", &e, true);
                return Err(e);
            }
            let need_inhibit = session_cfg.settings.display.always_on
                || session_cfg.settings.system_always_on;
            if let Err(e) = power_inhibitor.start(need_inhibit) {
                eprintln!("[Security] {e}");
            }
        }

        // 4.5. Pre-flight OS Camera Portal permission (org.freedesktop.portal.Camera):
        //      Request before fullscreen & kernel EVIOCGRAB lockdown so any first-time OS
        //      privacy prompt (KDE/GNOME) can be answered cleanly outside the locked shell.
        if session_cfg.settings.browser.media_capture_camera
            || session_cfg.settings.proctoring.enabled
        {
            splash.set_status("Checking OS camera permission...");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(seb_platform::request_camera_portal_permission());
            });
            let outcome = loop {
                match rx.try_recv() {
                    Ok(res) => break res,
                    Err(mpsc::TryRecvError::Empty) => {
                        dialogs::flush_gtk_events();
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        break seb_platform::CameraPortalOutcome::Unavailable;
                    }
                }
            };
            match outcome {
                seb_platform::CameraPortalOutcome::Granted => {
                    eprintln!("[Permissions] OS camera portal permission granted.");
                }
                seb_platform::CameraPortalOutcome::Cancelled
                | seb_platform::CameraPortalOutcome::Denied => {
                    eprintln!("[Permissions] OS camera portal permission denied/cancelled by user.");
                }
                seb_platform::CameraPortalOutcome::NoCamera
                | seb_platform::CameraPortalOutcome::Unavailable => {}
            }
        }

        // 5. Two-phase Anti-Cheat startup (AGENTS.md):
        //    Phase 1: Arm pkexec helper & run prohibited-process pre-flight dialog (Recheck/Quit)
        //             BEFORE grabbing keyboards via EVIOCGRAB!
        //    Phase 2: Activate kernel keyboard grab once clean!
        let mut lockdown = LockdownClient::new();
        if opts.strict_lockdown {
            splash.set_status("Starting privileged Anti-Cheat lockdown...");
            splash.close();
            if let Err(err) = lockdown.start_helper() {
                let _ = integrity_store.finish(true);
                dialogs::show_message(None, "Anti-Cheat Lockdown Required", &err, true);
                return Err(err);
            }

            let blacklist = build_blacklist(&session_cfg.settings);
            loop {
                let detected = match lockdown.scan_prohibited_processes(&blacklist) {
                    Ok(d) => d,
                    Err(e) => {
                        lockdown.stop();
                        let _ = integrity_store.finish(true);
                        return Err(e);
                    }
                };
                if detected.is_empty() {
                    break;
                }
                match dialogs::show_prohibited_processes_preflight(&detected) {
                    PreflightAction::Recheck => continue,
                    PreflightAction::Quit => {
                        lockdown.stop();
                        let _ = integrity_store.finish(true);
                        return Err(
                            "Exam start cancelled because prohibited applications were running."
                                .to_string(),
                        );
                    }
                }
            }

            let kb_policy = KeyboardLockPolicy::strict_linux(
                session_cfg
                    .settings
                    .user_interface
                    .action_center
                    .enable_action_center,
            );
            if let Err(err) = lockdown.activate_lockdown(&kb_policy, &blacklist) {
                lockdown.stop();
                let _ = integrity_store.finish(true);
                dialogs::show_message(None, "Anti-Cheat Lockdown Failed", &err, true);
                return Err(err);
            }
        } else {
            splash.set_status("Opening secure browser window...");
        }

        // 6. Build fullscreen frameless GTK3 shell window
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Safe Exam Browser");
        window.style_context().add_class("seb-shell");
        dialogs::make_window_frameless(&window);
        if let Some(pb) = assets::pixbuf_from_bytes(assets::ICON_SEB_PNG, 64, 64) {
            window.set_icon(Some(&pb));
        }
        window.fullscreen();
        if opts.strict_lockdown {
            window.set_keep_above(true);
            window.stick();
            window.set_skip_taskbar_hint(true);
            window.set_skip_pager_hint(true);
        }

        let outer_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let window_overlay = gtk::Overlay::new();
        window_overlay.add(&outer_box);
        window.add(&window_overlay);

        let overlay = gtk::Overlay::new();
        outer_box.pack_start(&overlay, true, true, 0);

        let workspace = gtk::Layout::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
        workspace.set_hexpand(true);
        workspace.set_vexpand(true);
        workspace.style_context().add_class("seb-workspace");
        overlay.add(&workspace);

        // Aero Snap preview overlay rendered ABOVE all windows (AGENTS.md)
        let snap_preview = gtk::Box::new(gtk::Orientation::Vertical, 0);
        snap_preview.style_context().add_class("seb-snap-preview");
        snap_preview.set_no_show_all(true);
        snap_preview.hide();
        overlay.add_overlay(&snap_preview);
        overlay.set_overlay_pass_through(&snap_preview, true);
        let snap_preview_rect: Rc<Cell<Option<PaneRect>>> = Rc::new(Cell::new(None));
        {
            let sp_widget: gtk::Widget = snap_preview.clone().upcast();
            let sp_rect = Rc::clone(&snap_preview_rect);
            overlay.connect_get_child_position(move |_, child| {
                if child == &sp_widget {
                    return sp_rect
                        .get()
                        .map(|r| gdk::Rectangle::new(r.x, r.y, r.w, r.h));
                }
                None
            });
        }

        // Centered Alt+Tab internal window switcher overlay
        let switcher = WindowSwitcherOverlay::new();
        overlay.add_overlay(&switcher.root);

        // Fullscreen Lock Screen overlay (hidden until SEB_FORCE_LOCK_SCREEN)
        let lock_screen = gtk::Box::new(gtk::Orientation::Vertical, 12);
        lock_screen.set_halign(gtk::Align::Fill);
        lock_screen.set_valign(gtk::Align::Fill);
        lock_screen.set_no_show_all(true);
        lock_screen.hide();
        overlay.add_overlay(&lock_screen);

        let platform = Rc::new(PlatformMonitor::start());
        let clipboard = IsolatedClipboard::new(session_cfg.settings.security.clipboard_policy);
        let app_manager = Rc::new(RefCell::new(ApplicationManager::initialize(&[])));

        let shell = Rc::new(Self {
            window: window.clone(),
            outer_box,
            window_overlay,
            workspace: workspace.clone(),
            snap_preview,
            snap_preview_rect,
            lock_screen,
            config: RefCell::new(session_cfg),
            current_admin_password_hash: RefCell::new(current_admin_hash),
            panes: RefCell::new(Vec::new()),
            pane_mru_order: RefCell::new(Vec::new()),
            next_pane_id: Cell::new(1),
            active_pane_id: Cell::new(0),
            taskbar: RefCell::new(None),
            action_center: RefCell::new(None),
            switcher,
            switcher_origin_pane_id: Cell::new(None),
            switcher_peeked_minimized_id: Cell::new(None),
            switcher_peek_gen: Cell::new(0),
            platform,
            clipboard,
            app_manager,
            web_context: RefCell::new(None),
            active_downloads: RefCell::new(Vec::new()),
            temp_download_dir: RefCell::new(None),
            temp_config_download_dir: RefCell::new(None),
            temp_web_profile_dir: RefCell::new(None),
            lockdown: RefCell::new(lockdown),
            power_inhibitor: RefCell::new(power_inhibitor),
            integrity_store: RefCell::new(integrity_store),
            proctoring: RefCell::new(None),
            last_proctoring_capture: RefCell::new(Instant::now() - Duration::from_secs(60)),
            pending_proctoring_action: RefCell::new(None),
            deferred_proctoring_scheduled: Cell::new(false),
            server_client: RefCell::new(active_server.clone()),
            server_rx: RefCell::new(None),
            pending_server_events: Arc::new(Mutex::new(PendingServerEvents::default())),
            server_wake_tx: RefCell::new(None),
            server_ping_stop: RefCell::new(None),
            last_detected_user_id: RefCell::new(String::new()),
            last_moodle_cookie_checked: RefCell::new(String::new()),
            live_proctoring_pane_id: Cell::new(None),
            last_battery_reported: Cell::new(None),
            logs: Rc::new(RefCell::new(initial_logs)),
            strict_lockdown: Cell::new(opts.strict_lockdown),
            dev_bypass: Cell::new(opts.dev_bypass),
            shutting_down: Cell::new(false),
            violation_dialog_active: Cell::new(false),
            reconfig_in_progress: Cell::new(false),
            quit_prompt_active: Cell::new(false),
            hand_raised: Cell::new(false),
            last_sidebar_toggle: RefCell::new(Instant::now() - Duration::from_secs(5)),
            last_focus_loss_warn: RefCell::new(Instant::now() - Duration::from_secs(5)),
            focus_restore_pending: Cell::new(false),
        });

        // Apply session (creates taskbar, action center, external apps, web context, main pane, proctoring)
        shell.apply_current_session(false);

        if let Some(srv) = active_server {
            if let Ok(mut q) = shell.pending_server_events.lock() {
                q.logs
                    .push_back(("Browser session started".to_string(), "INFO".to_string()));
            }
            shell.start_server_ping_thread(srv);
        }

        // Global keyboard shortcut & security filter inside SEB window
        shell.wire_global_shortcuts();

        // Focus-loss anti-cheat re-assertion guard during lockdown
        {
            let sh = Rc::clone(&shell);
            window.connect_is_active_notify(move |win| {
                if !win.is_active() && sh.clipboard.policy() != ClipboardPolicy::Allow {
                    clear_gtk_clipboards();
                }
                if win.is_active() {
                    sh.focus_restore_pending.set(false);
                    if !dialogs::is_modal_active() && !sh.switcher.is_visible() {
                        if let Some(p) = sh.active_pane() {
                            if !p.is_find_bar_visible() {
                                p.web_view.grab_focus();
                            }
                        }
                    }
                    return;
                }
                if sh.strict_lockdown.get()
                    && !sh.shutting_down.get()
                    && !sh.quit_prompt_active.get()
                    && !dialogs::is_modal_active()
                {
                    let now = Instant::now();
                    if now.duration_since(*sh.last_focus_loss_warn.borrow()) >= Duration::from_secs(2)
                    {
                        *sh.last_focus_loss_warn.borrow_mut() = now;
                        let msg =
                            "Main browser window lost focus during locked exam session.";
                        eprintln!("[AntiCheat] {msg}");
                        sh.log_to_server(msg, "WARNING");
                    }
                    if !sh.focus_restore_pending.replace(true) {
                        let w_c = win.clone();
                        let sh_c = Rc::clone(&sh);
                        glib::timeout_add_local_once(Duration::from_millis(80), move || {
                            sh_c.focus_restore_pending.set(false);
                            if !w_c.is_active()
                                && sh_c.strict_lockdown.get()
                                && !sh_c.shutting_down.get()
                                && !sh_c.quit_prompt_active.get()
                                && !dialogs::is_modal_active()
                            {
                                w_c.present();
                            }
                        });
                    }
                }
            });
        }

        // Monitor hotplug check via GDK Display
        if let Some(display) = gdk::Display::default() {
            let sh_add = Rc::clone(&shell);
            display.connect_monitor_added(move |_, _| sh_add.perform_runtime_security_check());
            let sh_rem = Rc::clone(&shell);
            display.connect_monitor_removed(move |_, _| sh_rem.perform_runtime_security_check());
        }

        // Prevent accidental WM close without quit password / confirmation
        {
            let sh = Rc::clone(&shell);
            window.connect_delete_event(move |_, _| {
                if sh.shutting_down.get() {
                    glib::Propagation::Proceed
                } else {
                    sh.request_quit(None, false);
                    glib::Propagation::Stop
                }
            });
        }

        // Sync panes ONLY when the workspace dimensions actually change (avoids feedback loops
        // and prevents snapping dragged windows back inside workspace bounds).
        {
            let sh = Rc::clone(&shell);
            let last_ws_size = Cell::new((0i32, 0i32));
            workspace.connect_size_allocate(move |ws, alloc| {
                let w = alloc.width();
                let h = alloc.height();
                if w < 100 || h < 100 || last_ws_size.get() == (w, h) {
                    return;
                }
                last_ws_size.set((w, h));
                for pane in sh.panes.borrow().iter() {
                    pane.on_workspace_resized(ws, w, h);
                }
            });
        }

        // 16ms tick: Lockdown IPC events (Alt+Tab switcher, violations) & SEB Server instructions
        {
            let sh = Rc::clone(&shell);
            glib::timeout_add_local(Duration::from_millis(16), move || {
                if sh.shutting_down.get() {
                    return glib::ControlFlow::Break;
                }
                sh.poll_lockdown_events();
                sh.poll_server_instructions();
                glib::ControlFlow::Continue
            });
        }

        // 50ms tick: OS Clipboard / PRIMARY clearing whenever ClipboardPolicy != Allow (C++ parity!)
        {
            let sh = Rc::clone(&shell);
            glib::timeout_add_local(Duration::from_millis(50), move || {
                if sh.shutting_down.get() {
                    return glib::ControlFlow::Break;
                }
                sh.clipboard.clear_os_clipboards_if_restricted();
                glib::ControlFlow::Continue
            });
        }

        // 1s tick: Clock, non-blocking platform snapshot sync, external apps, Action Center live refresh, proctoring interval
        {
            let sh = Rc::clone(&shell);
            glib::timeout_add_local(Duration::from_secs(1), move || {
                if sh.shutting_down.get() {
                    return glib::ControlFlow::Break;
                }
                sh.tick_one_second();
                glib::ControlFlow::Continue
            });
        }

        // 5s tick: Continuous Runtime Security Monitor (inhibitor, debugger, VM, display, prohibited processes)
        {
            let sh = Rc::clone(&shell);
            glib::timeout_add_local(Duration::from_secs(5), move || {
                if sh.shutting_down.get() {
                    return glib::ControlFlow::Break;
                }
                sh.perform_runtime_security_check();
                glib::ControlFlow::Continue
            });
        }

        splash.close();
        dialogs::register_main_overlay(&shell.window_overlay);
        window.show_all();
        window.present();
        if let Some(p) = shell.active_pane() {
            p.web_view.grab_focus();
        }
        Ok(shell)
    }

    fn wire_global_shortcuts(self: &Rc<Self>) {
        let sh = Rc::clone(self);
        self.window.connect_key_press_event(move |_, ev| {
            if dialogs::is_modal_active() {
                return glib::Propagation::Proceed;
            }
            let state = ev.state();
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let alt = state.contains(gdk::ModifierType::MOD1_MASK);
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            let key = ev.keyval();

            // Record non-repeating key interaction for Window-Only Screen Proctoring
            sh.notify_proctoring_user_action("Key pressed.");

            let settings = sh.config.borrow().settings.clone();
            let kb = &settings.keyboard;

            // Per-function key blocking (F1..F12 + always block F11 fullscreen toggle)
            if key == gdk::keys::constants::F11 {
                return glib::Propagation::Stop;
            }
            if (key == gdk::keys::constants::F1 && !kb.allow_f1)
                || (key == gdk::keys::constants::F2 && !kb.allow_f2)
                || (key == gdk::keys::constants::F3 && !kb.allow_f3)
                || (key == gdk::keys::constants::F4 && !kb.allow_f4)
                || (key == gdk::keys::constants::F5 && !kb.allow_f5)
                || (key == gdk::keys::constants::F6 && !kb.allow_f6)
                || (key == gdk::keys::constants::F7 && !kb.allow_f7)
                || (key == gdk::keys::constants::F8 && !kb.allow_f8)
                || (key == gdk::keys::constants::F9 && !kb.allow_f9)
                || (key == gdk::keys::constants::F10 && !kb.allow_f10)
                || (key == gdk::keys::constants::F12 && !kb.allow_f12)
            {
                return glib::Propagation::Stop;
            }

            // Action Center toggle: Super+A or Ctrl+Shift+A (translated by lockdown helper).
            // Never trigger on bare Windows/Super key press alone!
            let super_mod = state.intersects(
                gdk::ModifierType::SUPER_MASK
                    | gdk::ModifierType::META_MASK
                    | gdk::ModifierType::MOD4_MASK,
            );
            let is_a = matches!(key, gdk::keys::constants::A | gdk::keys::constants::a);
            if (ctrl && shift && !alt && is_a) || (super_mod && !ctrl && !alt && is_a)
            {
                if settings.user_interface.action_center.enable_action_center {
                    let now = Instant::now();
                    if now.duration_since(*sh.last_sidebar_toggle.borrow())
                        >= Duration::from_millis(200)
                    {
                        *sh.last_sidebar_toggle.borrow_mut() = now;
                        sh.toggle_action_center();
                    }
                }
                return glib::Propagation::Stop;
            }

            // Escape handling
            if key == gdk::keys::constants::Escape {
                if alt && !kb.allow_alt_esc {
                    return glib::Propagation::Stop;
                }
                if ctrl && !kb.allow_ctrl_esc {
                    return glib::Propagation::Stop;
                }
                if let Some(ref ac) = *sh.action_center.borrow() {
                    if ac.is_visible() {
                        ac.hide();
                        return glib::Propagation::Stop;
                    }
                }
                if let Some(ref tb) = *sh.taskbar.borrow() {
                    if tb.is_popup_visible() {
                        tb.hide_popup();
                        return glib::Propagation::Stop;
                    }
                }
                if sh.switcher.is_visible() {
                    sh.cancel_window_switcher();
                    return glib::Propagation::Stop;
                }
                if let Some(p) = sh.active_pane() {
                    if p.is_find_bar_visible() {
                        p.close_find_bar();
                        return glib::Propagation::Stop;
                    }
                }
                if !kb.allow_esc {
                    return glib::Propagation::Stop;
                }
            }

            // Swallow bare Super/Windows key presses inside SEB window
            if matches!(key, gdk::keys::constants::Super_L | gdk::keys::constants::Super_R) {
                return glib::Propagation::Stop;
            }
            // Alt+F4 & PrintScreen block at GTK level too (protects Dev Build fallback)
            if (alt && key == gdk::keys::constants::F4 && !kb.allow_alt_f4)
                || (key == gdk::keys::constants::Print && !kb.allow_print_screen)
            {
                return glib::Propagation::Stop;
            }

            // Quit: Ctrl+Q / Ctrl+Shift+Q
            if ctrl && (key == gdk::keys::constants::q || key == gdk::keys::constants::Q) {
                sh.request_quit(None, false);
                return glib::Propagation::Stop;
            }

            // Clipboard shortcuts (Ctrl+C/V/X, Ctrl+Insert, Shift+Delete, Shift+Insert)
            let is_copy = (ctrl && (key == gdk::keys::constants::c || key == gdk::keys::constants::C))
                || (ctrl && key == gdk::keys::constants::Insert);
            let is_cut = (ctrl && (key == gdk::keys::constants::x || key == gdk::keys::constants::X))
                || (shift && key == gdk::keys::constants::Delete);
            let is_paste = (ctrl && (key == gdk::keys::constants::v || key == gdk::keys::constants::V))
                || (shift && key == gdk::keys::constants::Insert);

            if is_copy || is_cut || is_paste {
                match settings.security.clipboard_policy {
                    ClipboardPolicy::Block => return glib::Propagation::Stop,
                    ClipboardPolicy::Isolated => {
                        if let Some(p) = sh.active_pane() {
                            if is_copy && kb.allow_ctrl_c {
                                p.exec_isolated_clipboard("copy");
                            } else if is_cut && kb.allow_ctrl_x {
                                p.exec_isolated_clipboard("cut");
                            } else if is_paste && kb.allow_ctrl_v {
                                p.exec_isolated_clipboard("paste");
                            }
                        }
                        return glib::Propagation::Stop;
                    }
                    ClipboardPolicy::Allow => {
                        if (is_copy && !kb.allow_ctrl_c)
                            || (is_cut && !kb.allow_ctrl_x)
                            || (is_paste && !kb.allow_ctrl_v)
                        {
                            return glib::Propagation::Stop;
                        }
                    }
                }
            }

            // Always-blocked browser shortcuts: Ctrl+U (source), Ctrl+S (save), Ctrl+O (open)
            if ctrl
                && matches!(
                    key,
                    gdk::keys::constants::u
                        | gdk::keys::constants::U
                        | gdk::keys::constants::s
                        | gdk::keys::constants::S
                        | gdk::keys::constants::o
                        | gdk::keys::constants::O
                )
            {
                return glib::Propagation::Stop;
            }

            // Print: Ctrl+P
            if ctrl && (key == gdk::keys::constants::p || key == gdk::keys::constants::P) {
                if settings.browser.allow_print {
                    if let Some(p) = sh.active_pane() {
                        p.print_page();
                    }
                }
                return glib::Propagation::Stop;
            }

            // DevTools: F12, Ctrl+Shift+I, Ctrl+Shift+J
            if key == gdk::keys::constants::F12
                || (ctrl
                    && shift
                    && matches!(
                        key,
                        gdk::keys::constants::i
                            | gdk::keys::constants::I
                            | gdk::keys::constants::j
                            | gdk::keys::constants::J
                    ))
            {
                if let Some(p) = sh.active_pane() {
                    p.open_dev_tools();
                }
                return glib::Propagation::Stop;
            }

            // Focus address bar: Ctrl+L
            if ctrl && (key == gdk::keys::constants::l || key == gdk::keys::constants::L) {
                if let Some(p) = sh.active_pane() {
                    p.focus_address_bar();
                }
                return glib::Propagation::Stop;
            }

            // Alt+F4: close active SEB internal window (Log -> popup pane -> request_quit with password)
            if alt && !ctrl && key == gdk::keys::constants::F4 {
                if !dialogs::is_modal_active() {
                    sh.close_active_window();
                }
                return glib::Propagation::Stop;
            }

            // Navigation: Alt+Left, Alt+Right, Alt+Home
            if alt && key == gdk::keys::constants::Left {
                if let Some(p) = sh.active_pane() {
                    p.go_back();
                }
                return glib::Propagation::Stop;
            }
            if alt && key == gdk::keys::constants::Right {
                if let Some(p) = sh.active_pane() {
                    p.go_forward();
                }
                return glib::Propagation::Stop;
            }
            if alt && key == gdk::keys::constants::Home {
                if let Some(p) = sh.active_pane() {
                    p.navigate_home();
                }
                return glib::Propagation::Stop;
            }

            // Find: Ctrl+F
            if ctrl && (key == gdk::keys::constants::f || key == gdk::keys::constants::F) {
                if settings.browser.allow_find {
                    if let Some(p) = sh.active_pane() {
                        p.toggle_find_bar();
                    }
                }
                return glib::Propagation::Stop;
            }

            // Reload: F5 / Ctrl+R (respects show_reload_warning!)
            if key == gdk::keys::constants::F5
                || (ctrl && (key == gdk::keys::constants::r || key == gdk::keys::constants::R))
            {
                if let Some(p) = sh.active_pane() {
                    p.reload();
                }
                return glib::Propagation::Stop;
            }

            // Page zoom: Ctrl+ / Ctrl- / Ctrl+0 (5% steps, 25%..300%)
            if ctrl
                && matches!(
                    key,
                    gdk::keys::constants::plus
                        | gdk::keys::constants::equal
                        | gdk::keys::constants::KP_Add
                )
            {
                if settings.browser.allow_page_zoom {
                    if let Some(p) = sh.active_pane() {
                        p.adjust_zoom_steps(1);
                    }
                }
                return glib::Propagation::Stop;
            }
            if ctrl
                && matches!(
                    key,
                    gdk::keys::constants::minus | gdk::keys::constants::KP_Subtract
                )
            {
                if settings.browser.allow_page_zoom {
                    if let Some(p) = sh.active_pane() {
                        p.adjust_zoom_steps(-1);
                    }
                }
                return glib::Propagation::Stop;
            }
            if ctrl && (key == gdk::keys::constants::_0 || key == gdk::keys::constants::KP_0) {
                if settings.browser.allow_page_zoom {
                    if let Some(p) = sh.active_pane() {
                        p.reset_zoom();
                    }
                }
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
    }

    fn apply_current_session(self: &Rc<Self>, preserve_existing_context: bool) {
        // Cancel any active downloads & terminate previous external apps
        self.cancel_all_downloads();
        self.app_manager.borrow_mut().terminate_all();
        dialogs::close_live_log_viewer();

        self.switcher.cancel();
        self.switcher_origin_pane_id.set(None);
        self.switcher_peeked_minimized_id.set(None);
        self.live_proctoring_pane_id.set(None);
        self.hand_raised.set(false);
        self.snap_preview.hide();
        self.lock_screen.hide();
        self.workspace.set_sensitive(true);

        for pane in self.panes.borrow_mut().drain(..) {
            self.workspace.remove(&pane.root);
            unsafe {
                pane.root.destroy();
            }
        }
        self.pane_mru_order.borrow_mut().clear();
        if let Some(tb) = self.taskbar.borrow_mut().take() {
            tb.remove_overlay(&self.window_overlay);
            self.outer_box.remove(&tb.root);
            unsafe {
                tb.root.destroy();
            }
        }
        if let Some(ac) = self.action_center.borrow_mut().take() {
            self.window_overlay.remove(&ac.root);
            unsafe {
                ac.root.destroy();
            }
        }
        if let Some(proc_client) = self.proctoring.borrow_mut().take() {
            proc_client.stop();
        }

        let cfg = self.config.borrow().clone();
        let settings = &cfg.settings;

        // Apply Desktop vs Mobile/Touch UI mode CSS class
        let win_ctx = self.window.style_context();
        if settings.user_interface.mode == UserInterfaceMode::Mobile {
            win_ctx.add_class("seb-mobile");
        } else {
            win_ctx.remove_class("seb-mobile");
        }

        // Apply clipboard policy & clear OS clipboards if restricted
        self.clipboard.set_policy(settings.security.clipboard_policy);

        // Apply audio settings
        if settings.audio.mute_audio {
            seb_platform::set_audio_muted(true);
            self.platform.update_audio_cached(Some(true), None);
        } else if settings.audio.initialize_volume {
            seb_platform::set_audio_volume(settings.audio.initial_volume);
            self.platform
                .update_audio_cached(None, Some(settings.audio.initial_volume));
        }

        // Prepare download/upload base directory (temp or configured or ~/Downloads)
        self.prepare_down_upload_directories(settings);

        // Initialize whitelisted external apps (auto-starts autostart=true apps!)
        *self.app_manager.borrow_mut() =
            ApplicationManager::initialize(&settings.applications.whitelist);

        // Build or reuse WebKitGTK WebContext (preserving login cookies when transitioning
        // out of a LoadWithBrowser Moodle SSO login session!)
        // C++ parity (app_controller.cpp): the browser profile (cookie jar) is kept when
        // transitioning from a LoadWithBrowser login page into the exam. A fresh WebContext is
        // still created so the web extension gets the NEW ConfigKey/BEK, but it SHARES the
        // existing WebsiteDataManager, which owns the cookies.
        let shared_wdm = if preserve_existing_context {
            self.web_context
                .borrow()
                .as_ref()
                .and_then(|c| c.website_data_manager())
        } else {
            None
        };
        let web_context = self.build_and_wire_web_context(&cfg, shared_wdm);
        *self.web_context.borrow_mut() = Some(web_context.clone());

        // Build bottom Taskbar if enabled
        if settings.user_interface.taskbar.enable_taskbar {
            let sh = Rc::clone(self);
            let tb = SebTaskbar::new(
                settings,
                Rc::clone(&self.platform),
                Rc::clone(&self.app_manager),
                &self.window_overlay,
                Rc::new(move |act| match act {
                    TaskbarAction::FocusWindow(id) => sh.focus_pane(id),
                    TaskbarAction::CloseWindow(id) => sh.close_pane(id),
                    TaskbarAction::ShowAbout => sh.show_about(),
                    TaskbarAction::ShowLog => {
                        dialogs::show_live_log_viewer(Some(&sh.window), Rc::clone(&sh.logs));
                    }
                    TaskbarAction::ShowVerificator => sh.show_verificator(),
                    TaskbarAction::RaiseHand(msg) => sh.handle_raise_hand(&msg),
                    TaskbarAction::LowerHand => sh.handle_lower_hand(),
                    TaskbarAction::ToggleOnScreenKeyboard => sh.toggle_on_screen_keyboard(),
                    TaskbarAction::RequestQuit => sh.request_quit(None, false),
                }),
            );
            self.outer_box.pack_end(&tb.root, false, false, 0);
            tb.root.show_all();
            *self.taskbar.borrow_mut() = Some(tb);
        }

        // Build Action Center sidebar overlay (with click-outside scrim & taskbar-parity popups!)
        if settings.user_interface.action_center.enable_action_center {
            let sh = Rc::clone(self);
            let ac = ActionCenterOverlay::new(
                settings,
                Rc::clone(&self.platform),
                Rc::new(move |act| match act {
                    ActionCenterTileAction::About => sh.show_about(),
                    ActionCenterTileAction::Log => {
                        dialogs::show_live_log_viewer(Some(&sh.window), Rc::clone(&sh.logs));
                    }
                    ActionCenterTileAction::Network => {
                        let tb_opt = sh.taskbar.borrow().clone();
                        if let Some(tb) = tb_opt {
                            tb.open_network_popup(&sh.platform);
                        }
                    }
                    ActionCenterTileAction::Audio => {
                        let tb_opt = sh.taskbar.borrow().clone();
                        let opened = tb_opt
                            .as_ref()
                            .is_some_and(|tb| tb.open_audio_popup(&sh.platform));
                        if !opened {
                            let st = sh.platform.snapshot().audio;
                            if st.available {
                                let next_muted = !st.muted;
                                seb_platform::set_audio_muted(next_muted);
                                sh.platform.update_audio_cached(Some(next_muted), None);
                            }
                        }
                    }
                    ActionCenterTileAction::Battery => {
                        let tb_opt = sh.taskbar.borrow().clone();
                        let opened = tb_opt.as_ref().is_some_and(|tb| tb.open_battery_popup());
                        if !opened {
                            let bat = sh.platform.snapshot().battery;
                            if bat.available {
                                let msg = if bat.charging {
                                    format!("Charging: {}%", bat.percentage)
                                } else {
                                    format!("Battery remaining: {}%", bat.percentage)
                                };
                                dialogs::show_message(Some(&sh.window), "Battery", &msg, false);
                            }
                        }
                    }
                    ActionCenterTileAction::Keyboard => {
                        let tb_opt = sh.taskbar.borrow().clone();
                        let opened = tb_opt
                            .as_ref()
                            .is_some_and(|tb| tb.open_keyboard_popup(&sh.platform));
                        if !opened {
                            let kb = sh.platform.snapshot().keyboard;
                            if kb.available_layouts.len() > 1 {
                                let cur = kb
                                    .available_layouts
                                    .iter()
                                    .position(|l| l.eq_ignore_ascii_case(&kb.current_short_name))
                                    .unwrap_or(0);
                                let next = (cur + 1) % kb.available_layouts.len();
                                let next_code = &kb.available_layouts[next];
                                if seb_platform::switch_keyboard_layout(next_code, next) {
                                    sh.platform.update_keyboard_cached(next_code);
                                }
                            }
                        }
                    }
                    ActionCenterTileAction::RaiseHand => {
                        if sh.hand_raised.get() {
                            sh.handle_lower_hand();
                        } else {
                            let force_msg = sh.config.borrow().settings.server.force_raise_hand_message;
                            if let Some(msg) = dialogs::prompt_text(
                                Some(&sh.window),
                                "Raise Hand",
                                if force_msg {
                                    "Enter a message for your invigilator (required):"
                                } else {
                                    "Optional message for your invigilator:"
                                },
                                "",
                            ) {
                                if !force_msg || !msg.trim().is_empty() {
                                    sh.handle_raise_hand(&msg);
                                }
                            }
                        }
                    }
                    ActionCenterTileAction::Proctoring => {
                        let status_opt = sh
                            .proctoring
                            .borrow()
                            .as_ref()
                            .map(|p| p.snapshot().1);
                        if let Some(status) = status_opt {
                            dialogs::show_message(
                                Some(&sh.window),
                                "Screen Proctoring",
                                &status,
                                false,
                            );
                        }
                    }
                    ActionCenterTileAction::OnScreenKeyboard => {
                        sh.toggle_on_screen_keyboard();
                    }
                    ActionCenterTileAction::Quit => sh.request_quit(None, false),
                }),
            );
            self.window_overlay.add_overlay(&ac.root);
            *self.action_center.borrow_mut() = Some(ac);
        }

        // Build properly joined Start URL (handling existing `?` query params cleanly!)
        let start_url = build_initial_start_url(settings);

        // Create Main BrowserPane (unlocked maximize!)
        self.spawn_pane(web_context, true, &start_url);

        // Start Window-Only Screen Proctoring ONLY if explicitly enabled in .seb config (AGENTS.md)
        if settings.proctoring.enabled
            && settings.proctoring.screen_proctoring.enabled
            && !settings
                .proctoring
                .screen_proctoring
                .service_url
                .trim()
                .is_empty()
        {
            self.start_screen_proctoring(None);
        }

        // Update lockdown helper policy if active
        if self.strict_lockdown.get() && self.lockdown.borrow().is_armed() {
            let kb_policy = KeyboardLockPolicy::strict_linux(
                settings.user_interface.action_center.enable_action_center,
            );
            let blacklist = build_blacklist(settings);
            self.lockdown.borrow().update_policy(&kb_policy, &blacklist);
        }
    }

    fn build_and_wire_web_context(
        self: &Rc<Self>,
        cfg: &SessionConfiguration,
        shared_wdm: Option<WebsiteDataManager>,
    ) -> WebContext {
        // Always use a non-ephemeral WebsiteDataManager with an actual SQLite cookie jar so
        // WebKitGTK does NOT enable Private Browsing cookie/storage partitioning that causes
        // Microsoft Entra / OIDC SSO flows to report blocked cookies.
        // When an ephemeral or clear-on-startup/shutdown profile is required, use an isolated
        // temporary directory removed cleanly on shutdown.
        let use_temp_profile = cfg.is_browser_resource
            || (cfg.settings.browser.delete_cookies_on_startup
                && cfg.settings.browser.delete_cookies_on_shutdown
                && cfg.settings.browser.delete_cache_on_shutdown);

        let (ctx, active_profile_dir) = if let Some(ref wdm) = shared_wdm {
            (WebContext::with_website_data_manager(wdm), None)
        } else if use_temp_profile {
            if let Some(old) = self.temp_web_profile_dir.borrow_mut().take() {
                let _ = fs::remove_dir_all(old);
            }
            let tmp_root = std::env::temp_dir().join(format!(
                "seb-linux-webprofile-{}-{}",
                std::process::id(),
                now_nanos()
            ));
            let data_dir = tmp_root.join("data");
            let cache_dir = tmp_root.join("cache");
            let _ = fs::create_dir_all(&data_dir);
            let _ = fs::create_dir_all(&cache_dir);
            *self.temp_web_profile_dir.borrow_mut() = Some(tmp_root);
            let wdm = WebsiteDataManager::builder()
                .base_data_directory(data_dir.to_string_lossy().as_ref())
                .base_cache_directory(cache_dir.to_string_lossy().as_ref())
                .build();
            (WebContext::with_website_data_manager(&wdm), Some(data_dir))
        } else {
            let profile_dir = &cfg.app_config.browser_profile_path;
            let _ = fs::create_dir_all(profile_dir);
            let _ = fs::create_dir_all(&cfg.app_config.browser_cache_path);
            if cfg.settings.browser.delete_cookies_on_startup {
                remove_cookie_files(profile_dir);
            }
            let wdm = WebsiteDataManager::builder()
                .base_data_directory(profile_dir.to_string_lossy().as_ref())
                .base_cache_directory(cfg.app_config.browser_cache_path.to_string_lossy().as_ref())
                .build();
            (
                WebContext::with_website_data_manager(&wdm),
                Some(profile_dir.clone()),
            )
        };

        let ctx_is_ephemeral = ctx
            .website_data_manager()
            .is_some_and(|w| w.is_ephemeral());

        if let Some(wdm) = ctx.website_data_manager() {
            // Disable WebKit ITP so third-party SSO flows (e.g. Microsoft Entra / OIDC / SAML)
            // do not get cookies blocked inside WebKitGTK.
            wdm.set_itp_enabled(false);
            wdm.set_persistent_credential_storage_enabled(!ctx_is_ephemeral);
            if cfg.settings.browser.proxy.policy == ProxyPolicy::Custom {
                apply_custom_proxy_to_wdm(&wdm, &cfg.settings.browser.proxy);
            }
        }

        if let Some(cm) = ctx.cookie_manager() {
            if !ctx_is_ephemeral {
                if let Some(ref dir) = active_profile_dir {
                    let cookie_file = dir.join("cookies.sqlite");
                    if cfg.settings.browser.delete_cookies_on_startup {
                        remove_cookie_files(dir);
                    }
                    cm.set_persistent_storage(
                        &cookie_file.to_string_lossy(),
                        CookiePersistentStorage::Sqlite,
                    );
                }
            }
            // Apply after storage setup so first-party and third-party SSO cookies stay allowed
            // (WebKitGTK defaults to NoThirdParty!).
            cm.set_accept_policy(CookieAcceptPolicy::Always);
        }

        let bcp47 = system_bcp47_locale();
        ctx.set_spell_checking_enabled(cfg.settings.browser.allow_spell_checking);
        if cfg.settings.browser.allow_spell_checking {
            ctx.set_spell_checking_languages(&[bcp47.as_str()]);
        }

        // Enable WebKitGTK favicon database so sidebar & Alt+Tab cards show each webpage's favicon
        if let Some(ref dir) = active_profile_dir {
            let fav_dir = dir.join("favicons");
            let _ = fs::create_dir_all(&fav_dir);
            ctx.set_favicon_database_directory(Some(&fav_dir.to_string_lossy()));
        } else {
            ctx.set_favicon_database_directory(None);
        }

        // Wire Pure Rust WebKitGTK Web Extension for per-request SEB headers & subresource filter!
        if let Some(ext_dir) = resolve_web_extension_dir() {
            let init_payload = serde_json::json!({
                "sendConfigurationKey": cfg.settings.browser.send_configuration_key,
                "sendBrowserExamKey": cfg.settings.browser.send_browser_exam_key,
                "configurationKey": cfg.settings.browser.configuration_key,
                "browserExamKey": compute_effective_bek(cfg),
                "acceptLanguage": bcp47,
                "filter": {
                    "processMainRequests": cfg.settings.browser.filter.process_main_requests,
                    "processContentRequests": cfg.settings.browser.filter.process_content_requests,
                    "rules": cfg.settings.browser.filter.rules,
                }
            })
            .to_string();

            ctx.connect_initialize_web_extensions(move |c| {
                c.set_web_extensions_directory(&ext_dir.to_string_lossy());
                let variant = glib::Variant::from(init_payload.as_str());
                c.set_web_extensions_initialization_user_data(&variant);
            });
        }

        // Complete WebKitGTK Download pipeline (authenticated .seb downloads + regular downloads!)
        let sh = Rc::clone(self);
        ctx.connect_download_started(move |_, download| {
            sh.handle_download_started(download);
        });

        ctx
    }

    fn handle_download_started(self: &Rc<Self>, download: &webkit2gtk::Download) {
        let settings = self.config.borrow().settings.clone();
        let req_uri = download
            .request()
            .and_then(|r| r.uri())
            .map(|s| s.to_string())
            .unwrap_or_default();

        self.active_downloads.borrow_mut().push(download.clone());
        self.log_to_server(&format!("Browser download requested: {req_uri}"), "INFO");

        let failed = Rc::new(Cell::new(false));
        let sh_dest = Rc::clone(self);
        let req_uri_c = req_uri.clone();
        let dl_c = download.clone();
        let failed_dest = Rc::clone(&failed);

        download.connect_decide_destination(move |dl, suggested| {
            let mime = dl
                .response()
                .and_then(|r| r.mime_type())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let matches_browser_resource = {
                let cfg = sh_dest.config.borrow();
                if cfg.is_browser_resource {
                    if let (Ok(req), Ok(start)) = (
                        Url::parse(&req_uri_c),
                        Url::parse(cfg.settings.browser.start_url.trim()),
                    ) {
                        req.host_str()
                            .zip(start.host_str())
                            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                            && req.path() == start.path()
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            let is_seb = matches_browser_resource
                || is_configuration_resource(&req_uri_c, suggested, &mime);

            if is_seb {
                if !settings.browser.allow_configuration_downloads
                    || !sh_dest.is_reconfiguration_allowed(&req_uri_c)
                {
                    failed_dest.set(true);
                    dl.cancel();
                    return true;
                }
                let cfg_dir = sh_dest.ensure_config_download_dir();
                let clean_name = sanitize_filename(suggested, "config.seb");
                let target_name = if clean_name.to_ascii_lowercase().ends_with(".seb") {
                    clean_name
                } else {
                    format!("{clean_name}.seb")
                };
                let target_path = unique_path_in_dir(&cfg_dir, &target_name);
                dl.set_destination(&format!("file://{}", target_path.to_string_lossy()));

                let sh_fin = Rc::clone(&sh_dest);
                let path_fin = target_path.clone();
                let dir_fin = cfg_dir.clone();
                let source_url_fin = req_uri_c.clone();
                let failed_fin = Rc::clone(&failed_dest);
                dl.connect_finished(move |d| {
                    sh_fin.active_downloads.borrow_mut().retain(|x| x != d);
                    if failed_fin.get() {
                        let _ = fs::remove_file(&path_fin);
                        return;
                    }
                    // Verify canonical containment inside temp config dir + `.seb` extension!
                    if let (Ok(canon_file), Ok(canon_dir)) =
                        (fs::canonicalize(&path_fin), fs::canonicalize(&dir_fin))
                    {
                        if canon_file.starts_with(&canon_dir)
                            && canon_file
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.eq_ignore_ascii_case("seb"))
                                .unwrap_or(false)
                        {
                            sh_fin.reconfigure_from_local_file(&canon_file, Some(&source_url_fin));
                        }
                    }
                });
                return true;
            }

            // Regular non-.seb download
            if !settings.browser.allow_downloads {
                failed_dest.set(true);
                dl.cancel();
                return true;
            }

            let base_dir = sh_dest.effective_down_upload_dir();
            let clean_name = sanitize_filename(suggested, "download");
            let dest_path = if settings.browser.allow_custom_down_upload_location {
                match dialogs::show_save_download_dialog(
                    Some(&sh_dest.window),
                    &base_dir,
                    &clean_name,
                ) {
                    Some(p) => p,
                    None => {
                        failed_dest.set(true);
                        dl.cancel();
                        return true;
                    }
                }
            } else {
                unique_path_in_dir(&base_dir, &clean_name)
            };

            dl.set_destination(&format!("file://{}", dest_path.to_string_lossy()));
            let sh_fin = Rc::clone(&sh_dest);
            let p_str = dest_path.to_string_lossy().to_string();
            let failed_fin = Rc::clone(&failed_dest);
            dl.connect_finished(move |d| {
                sh_fin.active_downloads.borrow_mut().retain(|x| x != d);
                if !failed_fin.get() {
                    sh_fin.log_to_server(&format!("Browser download completed: {p_str}"), "INFO");
                }
            });
            true
        });

        let sh_fail = Rc::clone(self);
        let failed_cb = Rc::clone(&failed);
        dl_c.connect_failed(move |d, err| {
            failed_cb.set(true);
            sh_fail.active_downloads.borrow_mut().retain(|x| x != d);
            sh_fail.log_to_server(&format!("Browser download failed: {err}"), "WARNING");
        });
    }

    fn is_reconfiguration_allowed(&self, url: &str) -> bool {
        let cfg = self.config.borrow();
        if cfg.is_browser_resource {
            return true;
        }
        let sec = &cfg.settings.security;
        if sec.quit_password_hash.is_empty() {
            return cfg.is_client_configuration || sec.allow_reconfiguration;
        }
        if !sec.allow_reconfiguration || sec.reconfiguration_url.trim().is_empty() {
            return false;
        }
        matches_reconfig_wildcard(url, sec.reconfiguration_url.trim())
    }

    fn prepare_down_upload_directories(&self, settings: &AppSettings) {
        if let Some(old) = self.temp_download_dir.borrow_mut().take() {
            let _ = fs::remove_dir_all(old);
        }
        if settings.browser.use_temporary_down_upload_directory {
            let tmp = std::env::temp_dir().join(format!(
                "seb-linux-downloads-{}-{}",
                std::process::id(),
                now_nanos()
            ));
            let _ = fs::create_dir_all(&tmp);
            *self.temp_download_dir.borrow_mut() = Some(tmp);
        }
    }

    fn effective_down_upload_dir(&self) -> PathBuf {
        if let Some(ref tmp) = *self.temp_download_dir.borrow() {
            return tmp.clone();
        }
        let configured = self.config.borrow().settings.browser.down_upload_directory.clone();
        if !configured.trim().is_empty() {
            let p = PathBuf::from(configured.trim());
            if p.is_dir() {
                return p;
            }
        }
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let dl = home.join("Downloads");
            if dl.is_dir() {
                return dl;
            }
            return home;
        }
        PathBuf::from("/tmp")
    }

    fn ensure_config_download_dir(&self) -> PathBuf {
        if let Some(ref existing) = *self.temp_config_download_dir.borrow() {
            let _ = fs::create_dir_all(existing);
            return existing.clone();
        }
        let dir = std::env::temp_dir().join(format!(
            "seb-linux-configuration-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let _ = fs::create_dir_all(&dir);
        *self.temp_config_download_dir.borrow_mut() = Some(dir.clone());
        dir
    }

    fn cancel_all_downloads(&self) {
        for dl in self.active_downloads.borrow_mut().drain(..) {
            dl.cancel();
        }
    }

    fn spawn_pane(self: &Rc<Self>, context: WebContext, is_main: bool, url: &str) {
        let cfg = self.config.borrow().clone();
        let settings = &cfg.settings;
        let win_settings = if is_main {
            settings.browser.main_window.clone()
        } else {
            settings.browser.additional_window.clone()
        };

        let bek = compute_effective_bek(&cfg);
        let ck = settings.browser.configuration_key.clone();
        let ua = build_user_agent(settings);
        let home_target = build_home_url(settings);
        let upload_dir = self.effective_down_upload_dir();

        let id = self.next_pane_id.get();
        self.next_pane_id.set(id + 1);

        let sh = Rc::clone(self);
        let ctx_for_popups = context.clone();
        let pane = BrowserPane::new(
            id,
            is_main,
            &self.window,
            &self.workspace,
            &context,
            settings,
            win_settings.clone(),
            bek,
            ck,
            ua,
            home_target,
            upload_dir,
            Rc::clone(&self.clipboard),
            url,
            Rc::new(move |act| match act {
                PaneAction::Focus(pid) => sh.focus_pane(pid),
                PaneAction::Close(pid) => sh.close_pane(pid),
                PaneAction::Minimize(pid) => sh.minimize_pane(pid),
                PaneAction::TitleOrUrlChanged { id: pid, title } => {
                    sh.sync_taskbar_windows();
                    if let Some(ref ac) = *sh.action_center.borrow() {
                        if ac.is_visible() {
                            let panes = sh.panes.borrow().clone();
                            let sh_f = Rc::clone(&sh);
                            ac.refresh_windows(&panes, Rc::new(move |id| sh_f.focus_pane(id)));
                        }
                    }
                    if pid == sh.active_pane_id.get() {
                        sh.notify_proctoring_user_action(&format!("Browser navigated: {title}"));
                    }
                }
                PaneAction::OpenPopup { url } => {
                    sh.spawn_pane(ctx_for_popups.clone(), false, &url);
                }
                PaneAction::ReconfigureDownload { pane_id, url } => {
                    sh.initiate_authenticated_reconfig_download(pane_id, &url);
                }
                PaneAction::QuitUrlTriggered { reset } => {
                    sh.handle_quit_url(reset);
                }
                PaneAction::SnapPreview(rect_opt) => {
                    sh.snap_preview_rect.set(rect_opt);
                    if rect_opt.is_some() {
                        sh.snap_preview.set_no_show_all(false);
                        sh.snap_preview.show();
                        sh.snap_preview.queue_resize();
                        if let Some(parent) = sh.snap_preview.parent() {
                            parent.queue_resize();
                        }
                    } else {
                        sh.snap_preview.hide();
                    }
                }
                PaneAction::UserInteraction(desc) => {
                    sh.notify_proctoring_user_action(&desc);
                }
                PaneAction::BroadcastClipboard { id: cid, b64 } => {
                    for p in sh.panes.borrow().iter() {
                        p.sync_isolated_clipboard(&cid, &b64);
                    }
                }
                PaneAction::LmsUserIdentifierDetected(uid) => {
                    sh.report_lms_user_identifier(&uid);
                }
                PaneAction::PageLoaded { url } => {
                    sh.detect_lms_user_identifier_from_cookies(&url);
                }
                PaneAction::OpenPdfInline { pane_id, url } => {
                    sh.open_pdf_inline(pane_id, &url);
                }
                PaneAction::Log(msg) => {
                    eprintln!("{msg}");
                    sh.logs.borrow_mut().push(msg);
                }
            }),
        );

        // Sync current isolated clipboard state into newly created pane
        if settings.security.clipboard_policy == ClipboardPolicy::Isolated {
            let (cid, b64) = self.clipboard.current_state();
            if !cid.is_empty() {
                pane.sync_isolated_clipboard(&cid, &b64);
            }
        }

        self.workspace.put(&pane.root, 0, 0);
        if is_main {
            pane.maximize(&self.workspace);
        } else {
            let ws_w = self.workspace.allocated_width().max(1024);
            let ws_h = self.workspace.allocated_height().max(700);
            let rect = BrowserPane::compute_initial_subwindow_rect(ws_w, ws_h, &win_settings);
            pane.set_windowed_rect(&self.workspace, rect, false);
        }
        pane.root.show_all();
        self.panes.borrow_mut().push(pane);
        self.active_pane_id.set(id);
        self.touch_pane_mru(id);
        self.sync_taskbar_windows();
        self.log_to_server("Browser window opened", "INFO");
    }

    fn initiate_authenticated_reconfig_download(self: &Rc<Self>, pane_id: u64, raw_url: &str) {
        if !self.is_reconfiguration_allowed(raw_url) {
            dialogs::show_message(
                Some(&self.window),
                "Reconfiguration Denied",
                "Reconfiguring Safe Exam Browser from this URL is not permitted by the current exam configuration.",
                true,
            );
            return;
        }
        let target_url = download_url(raw_url);
        // Download through WebKitGTK so authenticated browser session cookies (e.g. MoodleSession) are included!
        let pane_opt = self
            .panes
            .borrow()
            .iter()
            .find(|p| p.id == pane_id)
            .cloned()
            .or_else(|| self.active_pane());
        if let Some(pane) = pane_opt {
            pane.web_view.download_uri(&target_url);
        }
    }

    fn reconfigure_from_local_file(self: &Rc<Self>, path: &Path, source_url: Option<&str>) {
        if self.reconfig_in_progress.replace(true) {
            return;
        }
        let res_str = path.to_string_lossy().to_string();
        let splash = RuntimeSplash::show_over(
            Some(&self.window),
            SEB_APPLICATION_VERSION,
            "Reconfiguring Safe Exam Browser...",
        );
        let client_admin_hash = self.current_admin_password_hash.borrow().clone();
        let session_admin_hash = {
            let h = self.config.borrow().settings.security.admin_password_hash.clone();
            if h.is_empty() { client_admin_hash.clone() } else { h }
        };
        let was_browser_resource = self.config.borrow().is_browser_resource;

        let outcome = load_resource_with_password_sequence(&res_str, &session_admin_hash, true);
        match outcome {
            Ok(LoadOutcome::Loaded(mut loaded)) => {
                if let Some(src_url) = source_url {
                    apply_start_url_query_from_resource(src_url, &mut loaded.settings);
                }
                if !loaded.settings.browser.enable_browser {
                    splash.close();
                    self.reconfig_in_progress.set(false);
                    dialogs::show_message(
                        Some(&self.window),
                        "Reconfiguration Rejected",
                        "This configuration disables the browser.",
                        true,
                    );
                    return;
                }
                if loaded.settings.configuration_mode == ConfigurationMode::ConfigureClient {
                    splash.close();
                    if let Err(e) = verify_admin_password_for_client_config(
                        Some(&self.window),
                        &client_admin_hash,
                        &loaded,
                    ) {
                        self.reconfig_in_progress.set(false);
                        dialogs::show_message(Some(&self.window), "Reconfiguration Denied", &e, true);
                        return;
                    }
                    let dest = self.config.borrow().app_config.app_data_file_path.clone();
                    if let Err(e) = save_client_configuration(&loaded, &dest) {
                        self.reconfig_in_progress.set(false);
                        dialogs::show_message(Some(&self.window), "Save Failed", &e, true);
                        return;
                    }
                    *self.current_admin_password_hash.borrow_mut() =
                        loaded.settings.security.admin_password_hash.clone();
                }

                // CRITICAL PARITY (app_controller.cpp): preserve Moodle SSO login cookies
                // when transitioning from a LoadWithBrowser login page into the exam!
                if was_browser_resource {
                    loaded.settings.browser.delete_cookies_on_startup = false;
                }

                let mut next_cfg = self.config.borrow().clone();
                next_cfg.settings = loaded.settings;
                next_cfg.raw_seb_data = loaded.raw_seb_data;
                next_cfg.is_browser_resource = false;
                next_cfg.is_client_configuration =
                    next_cfg.settings.configuration_mode == ConfigurationMode::ConfigureClient;

                let mut next_server: Option<ArcServer> = None;
                if next_cfg.settings.session_mode == SessionMode::Server
                    && !next_cfg.settings.server.server_url.trim().is_empty()
                {
                    splash.set_status("Connecting to SEB Server...");
                    match run_server_workflow(&mut next_cfg, &splash, &session_admin_hash) {
                        Ok(srv) => next_server = srv,
                        Err(e) => {
                            splash.close();
                            self.reconfig_in_progress.set(false);
                            dialogs::show_message(Some(&self.window), "SEB Server Error", &e, true);
                            return;
                        }
                    }
                }

                if let Err(e) = validate_proxy_settings(&next_cfg.settings.browser.proxy) {
                    splash.close();
                    self.reconfig_in_progress.set(false);
                    dialogs::show_message(Some(&self.window), "Invalid Proxy", &e, true);
                    return;
                }

                if !self.dev_bypass.get() {
                    let sec_check = validate_runtime_security(
                        &next_cfg,
                        &mut self.integrity_store.borrow_mut(),
                        self.strict_lockdown.get(),
                    );
                    if let Err(e) = sec_check {
                        splash.close();
                        self.reconfig_in_progress.set(false);
                        dialogs::show_message(Some(&self.window), "Security Check Failed", &e, true);
                        return;
                    }
                }

                // Stop ping thread & disconnect previous SEB Server session before reconfiguring
                if let Some(stop) = self.server_ping_stop.borrow_mut().take() {
                    stop.store(true, Ordering::Relaxed);
                }
                *self.server_wake_tx.borrow_mut() = None;
                if let Some(srv) = self.server_client.borrow_mut().take() {
                    drain_pending_server_events(&srv, &self.pending_server_events);
                    if let Ok(mut g) = srv.lock() {
                        let _ = g.disconnect();
                    }
                }
                if let Ok(mut q) = self.pending_server_events.lock() {
                    *q = PendingServerEvents::default();
                }
                *self.server_rx.borrow_mut() = None;

                eprintln!("SEB configuration data is valid.");
                self.logs
                    .borrow_mut()
                    .push("SEB configuration data is valid.".to_string());
                // Exam -> exam: honor the OLD session's DeleteCookiesOnShutdown (C++ finalizeBrowser).
                if !was_browser_resource {
                    let (old_delete_cookies, old_delete_cache, old_profile_path) = {
                        let c = self.config.borrow();
                        (
                            c.settings.browser.delete_cookies_on_shutdown,
                            c.settings.browser.delete_cache_on_shutdown,
                            c.app_config.browser_profile_path.clone(),
                        )
                    };
                    if old_delete_cookies || old_delete_cache {
                        let mut types = WebsiteDataTypes::empty();
                        if old_delete_cookies {
                            types |= WebsiteDataTypes::COOKIES;
                        }
                        if old_delete_cache {
                            types |= WebsiteDataTypes::MEMORY_CACHE | WebsiteDataTypes::DISK_CACHE;
                        }
                        if let Some(ref ctx) = *self.web_context.borrow() {
                            if let Some(wdm) = ctx.website_data_manager() {
                                clear_wdm_data_sync(&wdm, types);
                            }
                        }
                        if old_delete_cookies {
                            remove_cookie_files(&old_profile_path);
                        }
                    }
                }
                *self.config.borrow_mut() = next_cfg;
                *self.server_client.borrow_mut() = next_server.clone();
                splash.set_status("Applying new exam configuration...");
                // NOTE: Rebuilding WebContext is needed so WebExtension gets the new ConfigKey/BEK!
                // When transitioning from Moodle SSO login, copy/preserve session if persistent,
                // or build fresh WebContext with new ConfigKey/BEK extension data.
                self.apply_current_session(was_browser_resource);
                splash.bring_to_front();
                splash.set_status("Starting exam session...");
                if let Some(srv) = next_server {
                    if let Ok(mut q) = self.pending_server_events.lock() {
                        q.logs.push_back((
                            "Browser session reconfigured".to_string(),
                            "INFO".to_string(),
                        ));
                    }
                    self.start_server_ping_thread(srv);
                }
                splash.finish_min_duration(600);
            }
            Ok(LoadOutcome::OpenInBrowser(url)) => {
                splash.close();
                if let Some(p) = self.active_pane() {
                    p.web_view.load_uri(&url);
                }
            }
            Err(err) => {
                splash.close();
                dialogs::show_message(Some(&self.window), "Reconfiguration Failed", &err, true);
            }
        }
        self.reconfig_in_progress.set(false);
    }

    fn active_pane(&self) -> Option<Rc<BrowserPane>> {
        let active = self.active_pane_id.get();
        let panes = self.panes.borrow();
        panes
            .iter()
            .find(|p| p.id == active)
            .or_else(|| panes.first())
            .cloned()
    }

    fn focus_pane(&self, id: u64) {
        let was_peeking = self.switcher_origin_pane_id.take().is_some();
        if id == dialogs::LIVE_LOG_SWITCHER_ID {
            dialogs::set_live_log_viewer_visible(true);
            return;
        }
        if dialogs::is_live_log_viewer_open() {
            dialogs::set_live_log_viewer_visible(false);
        }
        if let Some(min_id) = self.switcher_peeked_minimized_id.take() {
            if min_id != id {
                if let Some(min_pane) = self.panes.borrow().iter().find(|p| p.id == min_id) {
                    if min_pane.minimized.get() {
                        min_pane.root.hide();
                    }
                }
            }
        }
        let target = self.panes.borrow().iter().find(|p| p.id == id).cloned();
        if let Some(pane) = target {
            let changed = self.active_pane_id.get() != id;
            self.active_pane_id.set(id);
            self.touch_pane_mru(id);
            let was_min = pane.minimized.get();
            pane.set_minimized(false);
            // Raise pane's own GdkWindow without removing/re-adding from gtk::Layout,
            // preserving active titlebar drag & resize pointer grabs when switching windows.
            if (changed || was_min || was_peeking) && self.panes.borrow().len() > 1 {
                if let Some(gw) = pane.root.window() {
                    gw.raise();
                }
            }
            if was_min {
                pane.root.show();
            }
            if changed || was_min {
                pane.web_view.grab_focus();
            }
        }
        self.raise_floating_proctoring();
    }

    fn touch_pane_mru(&self, id: u64) {
        let mut mru = self.pane_mru_order.borrow_mut();
        mru.retain(|&x| x != id);
        mru.insert(0, id);
    }

    /// Returns all open panes ordered by Most Recently Used (last-used first) for Alt+Tab.
    fn panes_in_mru_order(&self) -> Vec<Rc<BrowserPane>> {
        let panes = self.panes.borrow();
        let mru = self.pane_mru_order.borrow();
        let mut ordered = Vec::with_capacity(panes.len());
        for &id in mru.iter() {
            if let Some(p) = panes.iter().find(|p| p.id == id) {
                ordered.push(Rc::clone(p));
            }
        }
        for p in panes.iter() {
            if !ordered.iter().any(|o| o.id == p.id) {
                ordered.push(Rc::clone(p));
            }
        }
        ordered
    }

    fn raise_floating_proctoring(&self) {
        if let Some(pid) = self.live_proctoring_pane_id.get() {
            if let Some(pp) = self.panes.borrow().iter().find(|p| p.id == pid) {
                if !pp.minimized.get() {
                    if let Some(gw) = pp.root.window() {
                        gw.raise();
                    }
                }
            }
        }
    }

    /// Debounced live window peek (140ms delay between window switches while Alt+Tab is held)
    /// so rapid Tab cycling stays smooth and only raises the window the user pauses on.
    fn schedule_peek_pane(self: &Rc<Self>, id: u64) {
        let next_gen = self.switcher_peek_gen.get().wrapping_add(1);
        self.switcher_peek_gen.set(next_gen);
        let sh = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(140), move || {
            if sh.switcher_peek_gen.get() == next_gen && sh.switcher.is_visible() {
                sh.peek_pane(id);
            }
        });
    }

    /// Temporarily bring the candidate window highlighted in Alt+Tab to the front
    /// behind the switcher overlay (live window peek) without committing focus or
    /// permanently unminimizing it until Alt is released.
    fn peek_pane(&self, id: u64) {
        if self.switcher_origin_pane_id.get().is_none() {
            let orig = if dialogs::is_live_log_viewer_visible() {
                dialogs::LIVE_LOG_SWITCHER_ID
            } else {
                self.active_pane_id.get()
            };
            self.switcher_origin_pane_id.set(Some(orig));
        }
        if id == dialogs::LIVE_LOG_SWITCHER_ID {
            dialogs::set_live_log_viewer_visible(true);
            return;
        }
        if dialogs::is_live_log_viewer_open() {
            dialogs::set_live_log_viewer_visible(false);
        }
        if let Some(prev_min_id) = self.switcher_peeked_minimized_id.get() {
            if prev_min_id != id {
                if let Some(prev_pane) = self.panes.borrow().iter().find(|p| p.id == prev_min_id) {
                    if prev_pane.minimized.get() {
                        prev_pane.root.hide();
                    }
                }
                self.switcher_peeked_minimized_id.set(None);
            }
        }
        let target = self.panes.borrow().iter().find(|p| p.id == id).cloned();
        if let Some(pane) = target {
            if pane.minimized.get() {
                pane.root.show();
                self.switcher_peeked_minimized_id.set(Some(id));
            }
            if let Some(gw) = pane.root.window() {
                gw.raise();
            }
        }
    }

    fn commit_window_switcher(&self) {
        self.switcher_peek_gen
            .set(self.switcher_peek_gen.get().wrapping_add(1));
        if let Some(id) = self.switcher.commit() {
            self.focus_pane(id);
        } else {
            self.cancel_window_switcher();
        }
    }

    fn cancel_window_switcher(&self) {
        self.switcher_peek_gen
            .set(self.switcher_peek_gen.get().wrapping_add(1));
        self.switcher.cancel();
        if let Some(min_id) = self.switcher_peeked_minimized_id.take() {
            if let Some(min_pane) = self.panes.borrow().iter().find(|p| p.id == min_id) {
                if min_pane.minimized.get() {
                    min_pane.root.hide();
                }
            }
        }
        if let Some(orig_id) = self.switcher_origin_pane_id.take() {
            if orig_id == dialogs::LIVE_LOG_SWITCHER_ID {
                dialogs::set_live_log_viewer_visible(true);
            } else {
                if dialogs::is_live_log_viewer_open() {
                    dialogs::set_live_log_viewer_visible(false);
                }
                if let Some(orig_pane) = self.panes.borrow().iter().find(|p| p.id == orig_id) {
                    if !orig_pane.minimized.get() {
                        if let Some(gw) = orig_pane.root.window() {
                            gw.raise();
                        }
                    }
                }
            }
        }
    }

    fn close_active_window(&self) {
        if dialogs::is_live_log_viewer_open() && dialogs::is_live_log_viewer_visible() {
            dialogs::close_live_log_viewer();
            return;
        }
        if let Some(p) = self.active_pane() {
            if !p.is_main {
                self.close_pane(p.id);
                return;
            }
        }
        self.request_quit(None, false);
    }

    fn minimize_pane(&self, id: u64) {
        if let Some(pane) = self.panes.borrow().iter().find(|p| p.id == id) {
            pane.set_minimized(true);
        }
    }

    fn close_pane(&self, id: u64) {
        let mut panes = self.panes.borrow_mut();
        if let Some(idx) = panes.iter().position(|p| p.id == id && !p.is_main) {
            let removed = panes.remove(idx);
            self.workspace.remove(&removed.root);
            unsafe {
                removed.root.destroy();
            }
            if self.live_proctoring_pane_id.get() == Some(id) {
                self.live_proctoring_pane_id.set(None);
            }
            self.pane_mru_order.borrow_mut().retain(|&x| x != id);
            let next_to_focus = if self.active_pane_id.get() == id {
                let mru = self.pane_mru_order.borrow();
                mru.iter()
                    .copied()
                    .find(|mid| panes.iter().any(|p| p.id == *mid && !p.minimized.get()))
                    .or_else(|| mru.first().copied())
                    .or_else(|| panes.iter().rev().find(|p| !p.minimized.get()).map(|p| p.id))
                    .or_else(|| panes.last().map(|p| p.id))
            } else {
                None
            };
            drop(panes);
            if let Some(nid) = next_to_focus {
                self.focus_pane(nid);
            }
            self.sync_taskbar_windows();
            self.log_to_server("Browser window closed", "INFO");
        }
    }

    fn sync_taskbar_windows(&self) {
        let items: Vec<(u64, String, bool)> = self
            .panes
            .borrow()
            .iter()
            .map(|p| (p.id, p.title(), p.is_main))
            .collect();
        if let Some(ref tb) = *self.taskbar.borrow() {
            tb.update_window_list(&items);
        }
    }

    fn toggle_action_center(self: &Rc<Self>) {
        if let Some(ref ac) = *self.action_center.borrow() {
            let panes = self.panes.borrow().clone();
            let snap = self.platform.snapshot();
            let sh = Rc::clone(self);
            ac.toggle(&panes, &snap, Rc::new(move |id| sh.focus_pane(id)));
        }
    }

    /// 100% parity with C++ `browser_window.cpp:873-888` & `seb_session.cpp:700-715`:
    /// - `reset_on_quit_url == true` -> navigate back to home URL (WITHOUT wiping session!)
    /// - `reset_on_quit_url == false` -> confirm if `confirm_quit_url`, then STILL respect quit password!
    fn handle_quit_url(&self, reset: bool) {
        if self.lock_screen.is_visible() {
            return;
        }
        if reset {
            if let Some(p) = self.active_pane() {
                let home = build_home_url(&self.config.borrow().settings);
                if !home.is_empty() {
                    p.web_view.load_uri(&home);
                }
            }
            return;
        }
        let confirm = self.config.borrow().settings.browser.confirm_quit_url;
        if confirm
            && !dialogs::show_confirm(
                Some(&self.window),
                "Quit Safe Exam Browser",
                "The exam page requested that Safe Exam Browser should quit. Continue?",
            )
        {
            return;
        }
        self.request_quit(
            Some("Enter the quit password to close Safe Exam Browser:"),
            confirm,
        );
    }

    fn request_quit(&self, custom_prompt: Option<&str>, already_confirmed: bool) {
        if self.shutting_down.get() || self.quit_prompt_active.replace(true) {
            return;
        }
        if self.switcher.is_visible() {
            self.cancel_window_switcher();
        }
        if let Some(ref ac) = *self.action_center.borrow() {
            if ac.is_visible() {
                ac.hide();
            }
        }
        if let Some(ref tb) = *self.taskbar.borrow() {
            if tb.is_popup_visible() {
                tb.hide_popup();
            }
        }
        let sec = self.config.borrow().settings.security.clone();
        if !sec.allow_termination {
            dialogs::show_message(
                Some(&self.window),
                "Quit Disabled",
                "Quitting Safe Exam Browser is disabled by the current exam configuration.",
                false,
            );
            self.quit_prompt_active.set(false);
            return;
        }
        if !sec.quit_password_hash.is_empty() {
            let prompt =
                custom_prompt.unwrap_or("Enter the quit password to close Safe Exam Browser:");
            loop {
                let Some(pw) =
                    dialogs::prompt_password(Some(&self.window), "Quit Safe Exam Browser", prompt)
                else {
                    self.quit_prompt_active.set(false);
                    return;
                };
                if verify_password_or_hash(&pw, &sec.quit_password_hash) {
                    self.quit_prompt_active.set(false);
                    self.shutdown();
                    return;
                }
                dialogs::show_message(
                    Some(&self.window),
                    "Invalid Password",
                    "The entered quit password is incorrect.",
                    true,
                );
            }
        } else if already_confirmed
            || dialogs::show_confirm(
                Some(&self.window),
                "Quit Safe Exam Browser",
                "Are you sure you want to quit Safe Exam Browser?",
            )
        {
            self.quit_prompt_active.set(false);
            self.shutdown();
        } else {
            self.quit_prompt_active.set(false);
        }
    }

    fn show_about(&self) {
        let text = {
            let cfg = self.config.borrow();
            format!(
                "Safe Exam Browser for Linux (Rust)\nVersion: {}\nSEB Compatibility: {}\nConfiguration Key: {}",
                cfg.app_config.program_build_version,
                cfg.app_config.seb_compatibility_version,
                if cfg.settings.browser.configuration_key.is_empty() {
                    "(none)"
                } else {
                    &cfg.settings.browser.configuration_key
                        [..16.min(cfg.settings.browser.configuration_key.len())]
                }
            )
        };
        dialogs::show_message(Some(&self.window), "About Safe Exam Browser", &text, false);
    }

    fn show_verificator(&self) {
        let (build_ver, compat_ver, cfg_key, bek) = {
            let cfg = self.config.borrow();
            (
                cfg.app_config.program_build_version.clone(),
                cfg.app_config.seb_compatibility_version.clone(),
                cfg.settings.browser.configuration_key.clone(),
                compute_effective_bek(&cfg),
            )
        };
        dialogs::show_verificator_dialog(
            Some(&self.window),
            &build_ver,
            &compat_ver,
            &cfg_key,
            &bek,
        );
    }

    fn handle_raise_hand(&self, message: &str) {
        let ok = if let Some(ref srv) = *self.server_client.borrow() {
            srv.lock()
                .map(|mut g| g.raise_hand(message).is_ok())
                .unwrap_or(false)
        } else {
            false
        };
        if ok {
            self.hand_raised.set(true);
            if let Some(ref tb) = *self.taskbar.borrow() {
                tb.set_hand_raised(true);
            }
            if let Some(ref ac) = *self.action_center.borrow() {
                ac.set_hand_raised(true);
            }
        }
    }

    fn handle_lower_hand(&self) {
        let ok = if let Some(ref srv) = *self.server_client.borrow() {
            srv.lock()
                .map(|mut g| g.lower_hand().is_ok())
                .unwrap_or(false)
        } else {
            false
        };
        if ok {
            self.hand_raised.set(false);
            if let Some(ref tb) = *self.taskbar.borrow() {
                tb.set_hand_raised(false);
            }
            if let Some(ref ac) = *self.action_center.borrow() {
                ac.set_hand_raised(false);
            }
        }
    }

    fn wake_server_worker(&self) {
        if let Some(ref tx) = *self.server_wake_tx.borrow() {
            let _ = tx.send(());
        }
    }

    fn log_to_server(&self, message: &str, severity: &str) {
        self.logs
            .borrow_mut()
            .push(format!("[{severity}] {message}"));
        if self.server_client.borrow().is_some() {
            if let Ok(mut q) = self.pending_server_events.lock() {
                if q.logs.len() >= 256 {
                    q.logs.pop_front();
                }
                q.logs
                    .push_back((message.to_string(), severity.to_string()));
            }
            self.wake_server_worker();
        }
    }

    fn start_screen_proctoring(&self, session_id_override: Option<String>) {
        let sp = self.config.borrow().settings.proctoring.screen_proctoring.clone();
        if sp.service_url.trim().is_empty() {
            return;
        }
        let cache_dir = self
            .config
            .borrow()
            .app_config
            .temporary_directory
            .join("sps-cache");
        let cfg = ProctoringClientConfig {
            service_url: sp.service_url,
            client_id: sp.client_id,
            client_secret: sp.client_secret,
            group_id: sp.group_id,
            session_id: session_id_override.unwrap_or_default(),
            encryption_secret: sp.encryption_secret,
            cache_dir: Some(cache_dir),
            cache_size: sp.cache_size,
            downscaling: sp.image_downscaling,
            quantization: match sp.image_quantization {
                ProctoringImageQuantization::BlackAndWhite1bpp => {
                    ImageQuantizationKind::BlackAndWhite1bpp
                }
                ProctoringImageQuantization::Grayscale2bpp => ImageQuantizationKind::Grayscale2bpp,
                ProctoringImageQuantization::Grayscale4bpp => ImageQuantizationKind::Grayscale4bpp,
                ProctoringImageQuantization::Grayscale8bpp => ImageQuantizationKind::Grayscale8bpp,
                ProctoringImageQuantization::Color8bpp => ImageQuantizationKind::Color8bpp,
                ProctoringImageQuantization::Color16bpp => ImageQuantizationKind::Color16bpp,
                ProctoringImageQuantization::Color24bpp => ImageQuantizationKind::Color24bpp,
            },
            format: match sp.image_format {
                ProctoringImageFormat::Bmp => ImageFormatKind::Bmp,
                ProctoringImageFormat::Gif => ImageFormatKind::Gif,
                ProctoringImageFormat::Jpg => ImageFormatKind::Jpg,
                ProctoringImageFormat::Png => ImageFormatKind::Png,
            },
        };
        match ScreenProctoringClient::start(cfg) {
            Ok(client) => {
                *self.proctoring.borrow_mut() = Some(Rc::new(client));
                self.log_to_server("Started window-only Screen Proctoring service.", "INFO");
                self.capture_proctoring_frame_now("Session started");
            }
            Err(e) => {
                self.log_to_server(&e, "ERROR");
            }
        }
    }

    /// Deferred user-action throttling matching C++ `ScreenProctoringService::notifyUserAction`:
    /// captures immediately if `IntervalMinimum` elapsed, otherwise schedules a one-shot timer!
    fn notify_proctoring_user_action(self: &Rc<Self>, action: &str) {
        if self.proctoring.borrow().is_none() {
            return;
        }
        let min_ms = self
            .config
            .borrow()
            .settings
            .proctoring
            .screen_proctoring
            .interval_minimum
            .max(250);
        let elapsed_ms = self.last_proctoring_capture.borrow().elapsed().as_millis() as u64;
        if elapsed_ms >= min_ms {
            *self.pending_proctoring_action.borrow_mut() = None;
            self.capture_proctoring_frame_now(action);
        } else {
            *self.pending_proctoring_action.borrow_mut() = Some(action.to_string());
            if !self.deferred_proctoring_scheduled.replace(true) {
                let remaining = (min_ms - elapsed_ms).max(50);
                let sh = Rc::clone(self);
                glib::timeout_add_local_once(Duration::from_millis(remaining), move || {
                    sh.deferred_proctoring_scheduled.set(false);
                    let act = sh
                        .pending_proctoring_action
                        .borrow_mut()
                        .take()
                        .unwrap_or_else(|| "User interaction".to_string());
                    sh.capture_proctoring_frame_now(&act);
                });
            }
        }
    }

    /// Captures ONLY the Safe Exam Browser GTK window surface (NEVER full OS screen).
    /// Composites active WebKitGTK `snapshot()` onto the GTK window Cairo surface so Wayland
    /// accelerated WebViews are captured faithfully alongside SEB chrome.
    fn capture_proctoring_frame_now(&self, trigger_action: &str) {
        if self.proctoring.borrow().is_none() {
            return;
        }

        let mut alloc = self.window.allocation();
        let w = alloc.width();
        let h = alloc.height();
        if w <= 0 || h <= 0 || !self.window.is_visible() {
            return;
        }
        // Ensure GTK allocation is up-to-date before Cairo draw so GTK never emits
        // `gtk_widget_draw: assertion '!widget->priv->alloc_needed' failed`.
        self.window.size_allocate(&mut alloc);
        let Ok(mut surface) = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h) else {
            return;
        };
        {
            let Ok(cr) = cairo::Context::new(&surface) else {
                return;
            };
            self.window.draw(&cr);
        }

        let sp = self.config.borrow().settings.proctoring.screen_proctoring.clone();
        let active = self.active_pane();
        let meta = MetadataInput {
            capture_application_data: sp.meta_data.capture_application_data,
            capture_browser_data: sp.meta_data.capture_browser_data,
            capture_window_title: sp.meta_data.capture_window_title,
            app_name: "Safe Exam Browser".to_string(),
            is_main_window: active.as_ref().map(|p| p.is_main).unwrap_or(true),
            window_title: active.as_ref().map(|p| p.title()).unwrap_or_default(),
            browser_url: active.as_ref().map(|p| p.url()).unwrap_or_default(),
            trigger_action: trigger_action.to_string(),
        };
        *self.last_proctoring_capture.borrow_mut() = Instant::now();

        let submit_surface = |surf: &mut cairo::ImageSurface, width: i32, height: i32| -> Option<Vec<u8>> {
            surf.flush();
            let stride = surf.stride() as usize;
            let data = surf.data().ok()?;
            let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
            for y in 0..(height as usize) {
                for x in 0..(width as usize) {
                    let src = y * stride + x * 4;
                    let dst = (y * (width as usize) + x) * 4;
                    rgba[dst] = data[src + 2];
                    rgba[dst + 1] = data[src + 1];
                    rgba[dst + 2] = data[src];
                    rgba[dst + 3] = data[src + 3];
                }
            }
            Some(rgba)
        };

        if let Some(ref pane) = active {
            if pane.root.is_visible() && pane.web_view.is_mapped() {
                if let Some((wx, wy)) = pane.web_view.translate_coordinates(&self.window, 0, 0) {
                    let wv_alloc = pane.web_view.allocation();
                    let wv_w = wv_alloc.width();
                    let wv_h = wv_alloc.height();
                    if wv_w > 0 && wv_h > 0 {
                        let client_opt = self.proctoring.borrow().clone();
                        let meta_c = meta.clone();
                        pane.web_view.snapshot(
                            webkit2gtk::SnapshotRegion::Visible,
                            webkit2gtk::SnapshotOptions::NONE,
                            gio::Cancellable::NONE,
                            move |res| {
                                let Some(client) = client_opt else { return };
                                if let Ok(wv_surf) = res {
                                    if let Ok(cr) = cairo::Context::new(&surface) {
                                        let _ = cr.set_source_surface(&wv_surf, wx as f64, wy as f64);
                                        let _ = cr.paint();
                                    }
                                }
                                if let Some(rgba) = submit_surface(&mut surface, w, h) {
                                    client.submit_window_rgba(&rgba, w as u32, h as u32, &meta_c);
                                }
                            },
                        );
                        return;
                    }
                }
            }
        }

        if let Some(rgba) = submit_surface(&mut surface, w, h) {
            if let Some(ref client) = *self.proctoring.borrow() {
            client.submit_window_rgba(&rgba, w as u32, h as u32, &meta);
        }
        }
    }

    fn perform_runtime_security_check(&self) {
        if self.dev_bypass.get() || self.shutting_down.get() {
            return;
        }
        let cfg = self.config.borrow().clone();
        if cfg.is_browser_resource {
            return;
        }

        let mut violation: Option<String> = None;
        if !self.power_inhibitor.borrow_mut().is_alive() {
            violation = Some("Power sleep/idle inhibitor terminated unexpectedly.".to_string());
        } else if is_debugger_attached() {
            violation = Some("Debugger attachment detected during exam session.".to_string());
        } else if cfg.settings.security.virtual_machine_policy == VirtualMachinePolicy::Deny
            && is_virtual_machine()
        {
            violation = Some("Virtual machine detected during exam session.".to_string());
        } else if let Err(err) = validate_display(
            cfg.settings.display.allowed_displays,
            cfg.settings.display.internal_display_only,
            cfg.settings.display.ignore_error,
            &collect_gdk_screen_names(),
        ) {
            violation = Some(err);
        } else if !self.strict_lockdown.get() {
            let bl = build_blacklist(&cfg.settings);
            let found = detect_prohibited_processes(&bl);
            if !found.is_empty() {
                violation = Some(format!(
                    "Prohibited applications detected: {}",
                    found.join(", ")
                ));
            }
        }

        if let Some(reason) = violation {
            if !self.violation_dialog_active.replace(true) {
                eprintln!("[Security] {reason}");
                self.log_to_server(&reason, "ERROR");
                self.lockdown.borrow_mut().stop();
                dialogs::show_message(
                    Some(&self.window),
                    "Security Violation",
                    &format!("{reason}\n\nSafe Exam Browser will now close."),
                    true,
                );
                self.shutdown();
            }
        }
    }

    fn poll_lockdown_events(self: &Rc<Self>) {
        let events = self.lockdown.borrow().poll_events();
        let mut idx = 0;
        while idx < events.len() {
            match &events[idx] {
                LockdownEvent::AltTabStep { backwards } => {
                    if dialogs::is_modal_active() {
                        idx += 1;
                        continue;
                    }
                    if let Some(ref ac) = *self.action_center.borrow() {
                        if ac.is_visible() {
                            ac.hide();
                        }
                    }
                    if let Some(ref tb) = *self.taskbar.borrow() {
                        if tb.is_popup_visible() {
                            tb.hide_popup();
                        }
                    }
                    // Coalesce rapid Alt+Tab step(s) + commit/cancel within the same poll batch
                    // to avoid synchronous Cairo thumbnail rendering when already committed/cancelled.
                    let mut net_steps: isize = if *backwards { -1 } else { 1 };
                    let mut terminal: Option<bool> = None; // Some(true)=commit, Some(false)=cancel
                    let mut next_idx = idx + 1;
                    while next_idx < events.len() {
                        match &events[next_idx] {
                            LockdownEvent::AltTabStep { backwards: b } => {
                                net_steps += if *b { -1 } else { 1 };
                                next_idx += 1;
                            }
                            LockdownEvent::AltTabCommit => {
                                terminal = Some(true);
                                next_idx += 1;
                                break;
                            }
                            LockdownEvent::AltTabCancel => {
                                terminal = Some(false);
                                next_idx += 1;
                                break;
                            }
                            _ => break,
                        }
                    }

                    let panes = self.panes_in_mru_order();
                    let current_active_id = if dialogs::is_live_log_viewer_visible() {
                        dialogs::LIVE_LOG_SWITCHER_ID
                    } else {
                        self.active_pane_id.get()
                    };
                    if terminal == Some(false) {
                        self.cancel_window_switcher();
                    } else if terminal == Some(true) && !self.switcher.is_visible() {
                        let mut ids: Vec<u64> = panes.iter().map(|p| p.id).collect();
                        if dialogs::is_live_log_viewer_open() {
                            ids.push(dialogs::LIVE_LOG_SWITCHER_ID);
                        }
                        if !ids.is_empty() {
                            let count = ids.len() as isize;
                            let active_pos = ids
                                .iter()
                                .position(|&id| id == current_active_id)
                                .unwrap_or(0) as isize;
                            let target_pos = (active_pos + net_steps).rem_euclid(count) as usize;
                            self.focus_pane(ids[target_pos]);
                        }
                    } else {
                        let count = net_steps.unsigned_abs().max(1);
                        let back = net_steps < 0;
                        let mut selected_id = None;
                        for _ in 0..count {
                            let sh = Rc::clone(self);
                            selected_id = self.switcher.step(
                                back,
                                &panes,
                                current_active_id,
                                Rc::new(move |id| sh.focus_pane(id)),
                            );
                        }
                        if terminal == Some(true) {
                            self.commit_window_switcher();
                        } else if let Some(id) = selected_id {
                            self.schedule_peek_pane(id);
                        }
                    }
                    idx = next_idx;
                    continue;
                }
                LockdownEvent::AltTabCommit => {
                    self.commit_window_switcher();
                }
                LockdownEvent::AltTabCancel => {
                    self.cancel_window_switcher();
                }
                LockdownEvent::NavBack => {
                    if !dialogs::is_modal_active() {
                        if let Some(p) = self.active_pane() {
                            p.go_back();
                        }
                    }
                }
                LockdownEvent::NavForward => {
                    if !dialogs::is_modal_active() {
                        if let Some(p) = self.active_pane() {
                            p.go_forward();
                        }
                    }
                }
                LockdownEvent::NavHome => {
                    if !dialogs::is_modal_active() {
                        if let Some(p) = self.active_pane() {
                            p.navigate_home();
                        }
                    }
                }
                LockdownEvent::CloseActiveWindow => {
                    if !dialogs::is_modal_active() {
                        self.close_active_window();
                    }
                }
                LockdownEvent::Violation { reason, .. } => {
                    if !self.shutting_down.get() && !self.violation_dialog_active.replace(true) {
                        eprintln!("[AntiCheat] {reason}");
                        self.log_to_server(reason, "ERROR");
                        self.lockdown.borrow_mut().stop();
                        dialogs::show_message(
                            Some(&self.window),
                            "Anti-Cheat Security Violation",
                            &format!("{reason}\n\nSafe Exam Browser will now close."),
                            true,
                        );
                        self.shutdown();
                    }
                }
                LockdownEvent::HelperTerminated { reason } => {
                    if !self.shutting_down.get() && !self.violation_dialog_active.replace(true) {
                        eprintln!("[AntiCheat] {reason}");
                        self.log_to_server(reason, "ERROR");
                        dialogs::show_message(
                            Some(&self.window),
                            "Anti-Cheat Lockdown Interrupted",
                            &format!("{reason}\n\nSafe Exam Browser will now close."),
                            true,
                        );
                        self.shutdown();
                    }
                }
            }
            idx += 1;
        }
    }

    fn start_server_ping_thread(&self, srv: ArcServer) {
        if let Some(prev_stop) = self.server_ping_stop.borrow_mut().take() {
            prev_stop.store(true, Ordering::Relaxed);
        }
        let interval_ms = self
            .config
            .borrow()
            .settings
            .server
            .ping_interval_ms
            .max(1000);
        let (tx, rx) = mpsc::channel();
        let (wake_tx, wake_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let queue = Arc::clone(&self.pending_server_events);
        *self.server_rx.borrow_mut() = Some(rx);
        *self.server_wake_tx.borrow_mut() = Some(wake_tx);
        *self.server_ping_stop.borrow_mut() = Some(stop);

        thread::spawn(move || {
            let interval = Duration::from_millis(interval_ms);
            drain_pending_server_events(&srv, &queue);
            let mut last_ping = Instant::now();
            while !stop_t.load(Ordering::Relaxed) {
                let wait_for = interval.saturating_sub(last_ping.elapsed());
                let _ = wake_rx.recv_timeout(wait_for);
                if stop_t.load(Ordering::Relaxed) {
                    break;
                }
                drain_pending_server_events(&srv, &queue);
                if last_ping.elapsed() >= interval {
                    last_ping = Instant::now();
                    let res = {
                        let Ok(mut guard) = srv.lock() else { break };
                        guard.send_ping()
                    };
                    if let Ok(outcome) = res {
                        match outcome {
                            PingOutcome::OkEmpty => {}
                            other => {
                                if tx.send(other).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    fn poll_server_instructions(self: &Rc<Self>) {
        let mut received = Vec::new();
        if let Some(ref rx) = *self.server_rx.borrow() {
            while let Ok(outcome) = rx.try_recv() {
                received.push(outcome);
            }
        }
        for outcome in received {
            match outcome {
                PingOutcome::OkEmpty => {}
                PingOutcome::TerminatedByServer(code) => {
                    self.log_to_server(
                        &format!("SEB Server terminated session (HTTP {code})."),
                        "ERROR",
                    );
                    self.shutdown();
                    return;
                }
                PingOutcome::Instruction(inst) => {
                    let mut should_confirm = true;
                    match inst.instruction {
                        5 => {
                            // SEB_QUIT
                            self.confirm_server_instruction(&inst.confirmation);
                            self.shutdown();
                            return;
                        }
                        1 => {
                            // SEB_FORCE_LOCK_SCREEN
                            self.show_server_lock_screen(&inst.message);
                        }
                        2 => {
                            // NOTIFICATION_CONFIRM (1=LockScreen, 2=Hand)
                            if inst.notification_type == 1 {
                                let matched = self
                                    .server_client
                                    .borrow()
                                    .as_ref()
                                    .and_then(|s| {
                                        s.lock()
                                            .ok()
                                            .map(|mut g| g.clear_lock_screen_if_matches(inst.notification_id))
                                    })
                                    .unwrap_or(false);
                                if matched {
                                    self.unlock_server_lock_screen();
                                }
                            } else if inst.notification_type == 2 {
                                let matched = self
                                    .server_client
                                    .borrow()
                                    .as_ref()
                                    .and_then(|s| {
                                        s.lock()
                                            .ok()
                                            .map(|mut g| g.clear_hand_if_matches(inst.notification_id))
                                    })
                                    .unwrap_or(false);
                                if matched {
                                    self.hand_raised.set(false);
                                    if let Some(ref tb) = *self.taskbar.borrow() {
                                        tb.set_hand_raised(false);
                                    }
                                    if let Some(ref ac) = *self.action_center.borrow() {
                                        ac.set_hand_raised(false);
                                    }
                                }
                            }
                        }
                        3 => {
                            // SEB_PROCTORING (provider: 1=JitsiMeet, 2=Zoom, 3=ScreenProctoring; method: 1=Join, 2=Leave)
                            match inst.proctoring.provider {
                                1 | 2 => {
                                    if inst.proctoring.method == 1 {
                                        {
                                            let mut cfg = self.config.borrow_mut();
                                            cfg.settings.proctoring.enabled = true;
                                            cfg.settings.proctoring.allow_chat = inst.allow_chat;
                                            cfg.settings.proctoring.receive_audio =
                                                inst.receive_audio;
                                            cfg.settings.proctoring.receive_video =
                                                inst.receive_video;
                                        }
                                        self.start_live_proctoring(&inst.proctoring);
                                    } else if inst.proctoring.method == 2 {
                                        self.stop_live_proctoring();
                                    }
                                }
                                3 => {
                                    if inst.proctoring.method == 1 {
                                        {
                                            let mut cfg = self.config.borrow_mut();
                                            let sp = &mut cfg.settings.proctoring.screen_proctoring;
                                            if !inst.proctoring.service_url.is_empty() {
                                                sp.service_url =
                                                    inst.proctoring.service_url.clone();
                                            }
                                            if !inst.proctoring.client_id.is_empty() {
                                                sp.client_id = inst.proctoring.client_id.clone();
                                            }
                                            if !inst.proctoring.client_secret.is_empty() {
                                                sp.client_secret =
                                                    inst.proctoring.client_secret.clone();
                                            }
                                            if !inst.proctoring.group_id.is_empty() {
                                                sp.group_id = inst.proctoring.group_id.clone();
                                            }
                                            if !inst.proctoring.encryption_secret.is_empty() {
                                                sp.encryption_secret =
                                                    inst.proctoring.encryption_secret.clone();
                                            }
                                            sp.enabled = true;
                                            cfg.settings.proctoring.enabled = true;
                                        }
                                        self.start_screen_proctoring(
                                            if inst.proctoring.session_id.is_empty() {
                                                None
                                            } else {
                                                Some(inst.proctoring.session_id.clone())
                                            },
                                        );
                                    } else if inst.proctoring.method == 2 {
                                        if let Some(p) = self.proctoring.borrow_mut().take() {
                                            p.stop();
                                        }
                                    }
                                }
                                _ => {
                                    should_confirm = false;
                                    self.log_to_server(
                                        "Received unknown SEB_PROCTORING provider.",
                                        "WARNING",
                                    );
                                }
                            }
                        }
                        4 => {
                            // SEB_RECONFIGURE_SETTINGS (runtime proctoring audio/video/chat flags)
                            {
                                let mut cfg = self.config.borrow_mut();
                                cfg.settings.proctoring.allow_chat = inst.allow_chat;
                                cfg.settings.proctoring.receive_audio = inst.receive_audio;
                                cfg.settings.proctoring.receive_video = inst.receive_video;
                            }
                            self.log_to_server(
                                &format!(
                                    "Proctoring settings reconfigured (chat={}, audio={}, video={}).",
                                    inst.allow_chat, inst.receive_audio, inst.receive_video
                                ),
                                "INFO",
                            );
                        }
                        _ => {}
                    }
                    if should_confirm {
                        self.confirm_server_instruction(&inst.confirmation);
                    }
                }
            }
        }
    }

    fn confirm_server_instruction(&self, token: &str) {
        if token.is_empty() {
            return;
        }
        if self.server_client.borrow().is_some() {
            if let Ok(mut q) = self.pending_server_events.lock() {
                q.confirmations.push_back(token.to_string());
            }
            self.wake_server_worker();
        }
    }

    fn report_lms_user_identifier(&self, uid: &str) {
        let clean = uid.trim();
        if clean.is_empty() || self.server_client.borrow().is_none() {
            return;
        }
        if *self.last_detected_user_id.borrow() == clean {
            return;
        }
        *self.last_detected_user_id.borrow_mut() = clean.to_string();
        if let Ok(mut q) = self.pending_server_events.lock() {
            q.user_identifier = Some(clean.to_string());
        }
        self.wake_server_worker();
    }

    fn detect_lms_user_identifier_from_cookies(self: &Rc<Self>, page_url: &str) {
        if self.server_client.borrow().is_none() {
            return;
        }
        let Ok(parsed) = url::Url::parse(page_url) else {
            return;
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            return;
        }
        let origin = parsed.origin().ascii_serialization();
        let Some(ctx) = self.web_context.borrow().clone() else {
            return;
        };
        let Some(cm) = ctx.cookie_manager() else {
            return;
        };
        let sh = Rc::clone(self);
        let url_owned = page_url.to_string();
        cm.cookies(&url_owned, gio::Cancellable::NONE, move |res| {
            let Ok(cookies) = res else { return };
            for mut cookie in cookies {
                let name = cookie
                    .name()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let value = cookie
                    .value()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if value.is_empty() {
                    continue;
                }
                // 1. Open edX: edx-user-info JSON cookie contains "username"
                if name.eq_ignore_ascii_case("edx-user-info") {
                    if let Some(uid) = parse_edx_user_info_cookie(&value) {
                        sh.report_lms_user_identifier(&uid);
                        return;
                    }
                }
                // 2. Moodle: MoodleSession* cookie -> async lookup via Moodle sebuser/sebserver endpoint
                if name.starts_with("MoodleSession") {
                    let key = format!("{origin}|{name}|{value}");
                    if *sh.last_moodle_cookie_checked.borrow() == key {
                        continue;
                    }
                    *sh.last_moodle_cookie_checked.borrow_mut() = key;
                    let origin_c = origin.clone();
                    let name_c = name.clone();
                    let val_c = value.clone();
                    let queue = Arc::clone(&sh.pending_server_events);
                    let wake_tx = sh.server_wake_tx.borrow().clone();
                    thread::spawn(move || {
                        if let Some(uid) =
                            seb_server::client::lookup_moodle_user_id(&origin_c, &name_c, &val_c)
                        {
                            if let Ok(mut q) = queue.lock() {
                                q.user_identifier = Some(uid);
                            }
                            if let Some(tx) = wake_tx {
                                let _ = tx.send(());
                            }
                        }
                    });
                }
            }
        });
    }

    fn start_live_proctoring(self: &Rc<Self>, data: &seb_server::ProctoringInstructionData) {
        let Some(ctx) = self.web_context.borrow().clone() else {
            return;
        };
        let target_url = match data.provider {
            1 => {
                // Jitsi Meet web session
                let base = data.service_url.trim().trim_end_matches('/');
                let room = data.room_name.trim().trim_start_matches('/');
                if base.is_empty() || room.is_empty() {
                    return;
                }
                let mut url = format!("{base}/{room}");
                if !data.token.trim().is_empty() {
                    url.push_str(&format!("?jwt={}", data.token.trim()));
                }
                let p = self.config.borrow().settings.proctoring.clone();
                url.push_str(&format!(
                    "#config.prejoinPageEnabled=false&config.startWithAudioMuted={}&config.startWithVideoMuted={}",
                    !p.receive_audio, !p.receive_video
                ));
                url
            }
            2 => {
                // Zoom Web Client fallback on Linux
                let meeting = data.meeting_number.trim();
                if meeting.is_empty() {
                    return;
                }
                let mut url = format!("https://app.zoom.us/wc/{meeting}/join");
                let mut sep = '?';
                if !data.password.trim().is_empty() {
                    url.push_str(&format!("{sep}pwd={}", data.password.trim()));
                    sep = '&';
                }
                if !data.user_name.trim().is_empty() {
                    let b64 = base64::engine::general_purpose::STANDARD
                        .encode(data.user_name.trim().as_bytes());
                    url.push_str(&format!("{sep}un={b64}"));
                }
                url
            }
            _ => return,
        };

        if let Some(existing_id) = self.live_proctoring_pane_id.get() {
            if let Some(pane) = self.panes.borrow().iter().find(|p| p.id == existing_id).cloned() {
                pane.web_view.load_uri(&target_url);
                self.focus_pane(existing_id);
                return;
            }
        }

        // Spawn compact floating ProctoringWindow (4.2): no browser toolbar/address bar,
        // positioned neatly in the bottom-right corner of the workspace.
        let orig_additional = self.config.borrow().settings.browser.additional_window.clone();
        {
            let mut cfg = self.config.borrow_mut();
            let aw = &mut cfg.settings.browser.additional_window;
            aw.show_toolbar = false;
            aw.allow_address_bar = false;
            aw.show_reload_button = false;
            aw.show_home_button = false;
            aw.allow_backward_navigation = false;
            aw.allow_forward_navigation = false;
            aw.always_on_top = true;
            aw.full_screen_mode = false;
        }
        self.spawn_pane(ctx, false, &target_url);
        {
            self.config.borrow_mut().settings.browser.additional_window = orig_additional;
        }
        let new_id = self.active_pane_id.get();
        if let Some(pane) = self.panes.borrow().iter().find(|p| p.id == new_id).cloned() {
            let ws_w = self.workspace.allocated_width().max(1024);
            let ws_h = self.workspace.allocated_height().max(700);
            let pw = 260;
            let ph = 195;
            pane.enable_floating_proctoring_mode(&self.workspace);
            pane.set_windowed_rect(
                &self.workspace,
                PaneRect {
                    x: (ws_w - pw - 16).max(12),
                    y: (ws_h - ph - 16).max(12),
                    w: pw,
                    h: ph,
                },
                false,
            );
            if let Some(gw) = pane.root.window() {
                gw.raise();
            }
        }
        self.live_proctoring_pane_id.set(Some(new_id));
        self.log_to_server("Floating live proctoring window opened.", "INFO");
    }

    fn toggle_on_screen_keyboard(&self) {
        for candidate in ["onboard", "maliit-keyboard", "squeekboard", "florence"] {
            if std::process::Command::new(candidate)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
        dialogs::show_message(
            Some(&self.window),
            "On-Screen Keyboard",
            "No supported Linux on-screen keyboard found (onboard, maliit-keyboard, squeekboard, or florence).",
            false,
        );
    }

    fn stop_live_proctoring(&self) {
        if let Some(pid) = self.live_proctoring_pane_id.take() {
            self.close_pane(pid);
            self.log_to_server("Live proctoring window closed.", "INFO");
        }
    }

    fn open_pdf_inline(self: &Rc<Self>, pane_id: u64, pdf_url: &str) {
        let Some(pane) = self.panes.borrow().iter().find(|p| p.id == pane_id).cloned() else {
            return;
        };
        let show_toolbar = self.config.borrow().settings.browser.allow_pdf_reader_toolbar;
        let allow_print = self.config.borrow().settings.browser.allow_print;
        let Some(ctx) = self.web_context.borrow().clone() else {
            return;
        };
        // Fetch PDF with current WebKit session cookies via CookieManager, then render pages inline
        if let Some(cm) = ctx.cookie_manager() {
            let tmp_dir = self
                .config
                .borrow()
                .app_config
                .temporary_directory
                .join(format!("pdf-view-{}-{}", std::process::id(), now_nanos()));
            let _ = fs::create_dir_all(&tmp_dir);
            let pdf_path = tmp_dir.join("document.pdf");
            let pdf_url_owned = pdf_url.to_string();
            cm.cookies(pdf_url, gio::Cancellable::NONE, move |res| {
                let mut cookie_pairs = Vec::new();
                if let Ok(cookies) = res {
                    for mut c in cookies {
                        if let (Some(n), Some(v)) = (c.name(), c.value()) {
                            cookie_pairs.push(format!("{n}={v}"));
                        }
                    }
                }
                let cookie_hdr = cookie_pairs.join("; ");
                let (tx, rx) = mpsc::channel::<Result<String, String>>();
                let url_t = pdf_url_owned.clone();
                let dir_t = tmp_dir.clone();
                let pdf_t = pdf_path.clone();
                thread::spawn(move || {
                    let out = render_pdf_to_inline_html(
                        &url_t,
                        &cookie_hdr,
                        &dir_t,
                        &pdf_t,
                        show_toolbar,
                        allow_print,
                    );
                    let _ = tx.send(out);
                });
                let pane_c = Rc::clone(&pane);
                let base_url = pdf_url_owned.clone();
                glib::timeout_add_local(Duration::from_millis(80), move || match rx.try_recv() {
                    Ok(Ok(html)) => {
                        pane_c.web_view.load_html(&html, Some(&base_url));
                        glib::ControlFlow::Break
                    }
                    Ok(Err(err)) => {
                        let fallback = format!(
                            "<!doctype html><html><body style=\"font-family:sans-serif;background:#f0f0f0;color:#000;padding:24px;\"><h3>PDF Viewer</h3><p>{}</p></body></html>",
                            html_escape(&err)
                        );
                        pane_c.web_view.load_html(&fallback, Some(&base_url));
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
                });
            });
        }
    }

    /// 100% parity with C++ `seb_session.cpp:178-226`:
    /// Disables browser workspace, sends `<lockscreen>` NOTIFICATION to SEB Server,
    /// displays modal lock screen with "Confirm" button sending `NOTIFICATION_CONFIRMED`.
    fn show_server_lock_screen(&self, message: &str) {
        self.workspace.set_sensitive(false);
        if let Some(ref srv) = *self.server_client.borrow() {
            if let Ok(mut g) = srv.lock() {
                let _ = g.send_lock_screen_notification(message);
                g.log_event("Session locked by SEB Server", "WARNING");
            }
        }

        for c in self.lock_screen.children() {
            self.lock_screen.remove(&c);
        }
        let bg = self
            .config
            .borrow()
            .settings
            .user_interface
            .lock_screen
            .background_color
            .clone();
        let css = gtk::CssProvider::new();
        let _ = css.load_from_data(
            format!("box {{ background-color: {bg}; color: white; }}").as_bytes(),
        );
        self.lock_screen
            .style_context()
            .add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

        let spacer1 = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let lbl = gtk::Label::new(Some(if message.is_empty() {
            "Safe Exam Browser has been locked by the exam supervisor."
        } else {
            message
        }));
        lbl.set_line_wrap(true);
        let confirm_btn = gtk::Button::with_label("Confirm");
        confirm_btn.set_halign(gtk::Align::Center);
        confirm_btn.set_size_request(140, 34);
        let spacer2 = gtk::Box::new(gtk::Orientation::Vertical, 0);

        self.lock_screen.pack_start(&spacer1, true, true, 0);
        self.lock_screen.pack_start(&lbl, false, false, 10);
        self.lock_screen.pack_start(&confirm_btn, false, false, 8);
        self.lock_screen.pack_start(&spacer2, true, true, 0);

        let srv_opt = self.server_client.borrow().clone();
        let ls = self.lock_screen.clone();
        let ws = self.workspace.clone();
        confirm_btn.connect_clicked(move |b| {
            b.set_sensitive(false);
            if let Some(ref srv) = srv_opt {
                if let Ok(mut g) = srv.lock() {
                    let _ = g.confirm_lock_screen();
                    g.log_event("Lock screen confirmed by user", "INFO");
                }
            }
            ls.hide();
            ws.set_sensitive(true);
        });
        self.lock_screen.set_no_show_all(false);
        self.lock_screen.show_all();
    }

    fn unlock_server_lock_screen(&self) {
        self.lock_screen.hide();
        self.workspace.set_sensitive(true);
        self.log_to_server("Session unlocked by SEB Server", "INFO");
    }

    fn tick_one_second(self: &Rc<Self>) {
        let snap = self.platform.snapshot();

        // Poll external application windows & update taskbar active bars
        {
            let mut mgr = self.app_manager.borrow_mut();
            mgr.poll_all();
            if let Some(ref tb) = *self.taskbar.borrow() {
                tb.update_external_app_indicators(&mgr);
            }
        }

        // Update taskbar & Action Center indicators from non-blocking background snapshot
        let show_proc_btn = self
            .config
            .borrow()
            .settings
            .proctoring
            .show_taskbar_notification;
        let proc_snap = self.proctoring.borrow().as_ref().map(|p| p.snapshot());
        if let Some(ref tb) = *self.taskbar.borrow() {
            tb.apply_snapshot(&snap);
            if let Some((st, ref txt, _, _)) = proc_snap {
                tb.update_proctoring_status(st, txt, show_proc_btn);
            } else {
                tb.update_proctoring_status(ProctoringState::Inactive, "", false);
            }
        }

        // Live refresh Action Center clock, proctoring & system tiles while open!
        if let Some(ref ac) = *self.action_center.borrow() {
            if let Some((st, ref txt, _, _)) = proc_snap {
                ac.update_proctoring_status(st, txt, show_proc_btn);
            } else {
                ac.update_proctoring_status(ProctoringState::Inactive, "", false);
            }
            if ac.is_visible() {
                ac.refresh_tiles(&snap);
            }
        }

        // Forward Screen Proctoring logs to SEB Server & trigger interval captures
        let proctoring_logs = self
            .proctoring
            .borrow()
            .as_ref()
            .map(|p| p.drain_logs())
            .unwrap_or_default();
        for (msg, sev) in proctoring_logs {
            self.log_to_server(&msg, &sev);
        }

        let max_ms = self
            .config
            .borrow()
            .settings
            .proctoring
            .screen_proctoring
            .interval_maximum
            .max(1000);
        if self.proctoring.borrow().is_some()
            && self.last_proctoring_capture.borrow().elapsed() >= Duration::from_millis(max_ms)
        {
            self.capture_proctoring_frame_now("Interval");
        }

        // Enqueue Battery & Wi-Fi telemetry to SEB Server non-blockingly
        if self.server_client.borrow().is_some() {
            let mut enqueued = false;
            if let Ok(mut q) = self.pending_server_events.lock() {
                if snap.battery.available {
                    let cur = (snap.battery.percentage, snap.battery.power_connected);
                    if self.last_battery_reported.get() != Some(cur) {
                        self.last_battery_reported.set(Some(cur));
                        q.battery = Some(cur);
                        enqueued = true;
                    }
                }
                if snap.network.kind == 2 {
                    let sig = if snap.network.status == 2 {
                        snap.network
                            .networks
                            .iter()
                            .find(|n| n.active)
                            .map(|n| n.signal_percent)
                    } else {
                        None
                    };
                    q.wireless = Some(sig);
                    enqueued = true;
                }
            }
            if enqueued {
                self.wake_server_worker();
            }
        }
    }

    pub fn shutdown(&self) {
        if self.shutting_down.replace(true) {
            return;
        }
        let mut clean_shutdown = true;

        self.cancel_all_downloads();
        self.app_manager.borrow_mut().terminate_all();

        if let Some(p) = self.proctoring.borrow_mut().take() {
            if p.pending_count() > 0 {
                p.begin_flush();
                flush_proctoring_with_dialog(&self.window, &p);
            }
            p.stop();
        }
        if let Some(stop) = self.server_ping_stop.borrow_mut().take() {
            stop.store(true, Ordering::Relaxed);
        }
        *self.server_wake_tx.borrow_mut() = None;
        if let Some(srv) = self.server_client.borrow_mut().take() {
            drain_pending_server_events(&srv, &self.pending_server_events);
            if let Ok(mut guard) = srv.lock() {
                if !guard.disconnect() {
                    clean_shutdown = false;
                }
            }
        }

        // Clear browser cookies & HTTP cache on shutdown cleanly BEFORE quitting GTK main loop
        dialogs::close_live_log_viewer();
        for pane in self.panes.borrow_mut().drain(..) {
            self.workspace.remove(&pane.root);
            unsafe {
                pane.root.destroy();
            }
        }
        self.pane_mru_order.borrow_mut().clear();
        if let Some(tb) = self.taskbar.borrow_mut().take() {
            tb.remove_overlay(&self.window_overlay);
            self.outer_box.remove(&tb.root);
            unsafe {
                tb.root.destroy();
            }
        }
        if let Some(ac) = self.action_center.borrow_mut().take() {
            self.window_overlay.remove(&ac.root);
            unsafe {
                ac.root.destroy();
            }
        }
        let (is_browser_res, bcfg, profile_dir) = {
            let c = self.config.borrow();
            (
                c.is_browser_resource,
                c.settings.browser.clone(),
                c.app_config.browser_profile_path.clone(),
            )
        };
        if !is_browser_res && (bcfg.delete_cookies_on_shutdown || bcfg.delete_cache_on_shutdown) {
            let mut types = WebsiteDataTypes::empty();
            if bcfg.delete_cookies_on_shutdown {
                types |= WebsiteDataTypes::COOKIES;
            }
            if bcfg.delete_cache_on_shutdown {
                types |= WebsiteDataTypes::MEMORY_CACHE | WebsiteDataTypes::DISK_CACHE;
            }
            if let Some(ref ctx) = *self.web_context.borrow() {
                if let Some(wdm) = ctx.website_data_manager() {
                    clear_wdm_data_sync(&wdm, types);
                }
            }
            if bcfg.delete_cookies_on_shutdown {
                remove_cookie_files(&profile_dir);
            }
        }

        if self.clipboard.policy() != ClipboardPolicy::Allow {
            clear_gtk_clipboards();
        }
        if let Some(dir) = self.temp_download_dir.borrow_mut().take() {
            let _ = fs::remove_dir_all(dir);
        }
        if let Some(dir) = self.temp_config_download_dir.borrow_mut().take() {
            let _ = fs::remove_dir_all(dir);
        }
        if let Some(dir) = self.temp_web_profile_dir.borrow_mut().take() {
            let _ = fs::remove_dir_all(dir);
        }

        self.power_inhibitor.borrow_mut().stop();
        let _ = self.integrity_store.borrow_mut().finish(clean_shutdown);
        self.lockdown.borrow_mut().stop();
        self.window.close();
        gtk::main_quit();
    }
}

// =========================================================================
// Helper Functions
// =========================================================================

enum LoadOutcome {
    Loaded(LoadedConfiguration),
    OpenInBrowser(String),
}

/// 100% parity with C++ `loadSettingsWithPassword`:
/// 1. Try empty password hash (`pwcc` default client config) silently
/// 2. Try `current_password_hash` silently (if non-empty)
/// 3. Prompt user up to 5 times if `allow_prompt` is true
fn load_resource_with_password_sequence(
    resource: &str,
    current_password_hash: &str,
    allow_prompt: bool,
) -> Result<LoadOutcome, String> {
    let empty_hash = sha256_hex(b"");
    for (pw, is_hash) in [
        (Some(empty_hash.as_str()), true),
        (
            if current_password_hash.is_empty() {
                None
            } else {
                Some(current_password_hash)
            },
            true,
        ),
    ] {
        let attempt = load_from_resource(resource, pw, is_hash);
        match attempt.status {
            LoadStatus::Success => return Ok(LoadOutcome::Loaded(attempt)),
            LoadStatus::LoadWithBrowser => {
                return Ok(LoadOutcome::OpenInBrowser(
                    attempt.redirect_url.unwrap_or_else(|| resource.to_string()),
                ));
            }
            LoadStatus::PasswordNeeded => continue,
            _ => return Err(attempt.message),
        }
    }

    if !allow_prompt {
        return Err("The SEB configuration requires a password.".to_string());
    }

    for _ in 0..5 {
        let Some(pw) = dialogs::prompt_password(
            None,
            "SEB Configuration Password",
            "Enter the password to decrypt the SEB configuration:",
        ) else {
            return Err("Configuration password entry cancelled.".to_string());
        };
        let attempt = load_from_resource(resource, Some(&pw), false);
        match attempt.status {
            LoadStatus::Success => return Ok(LoadOutcome::Loaded(attempt)),
            LoadStatus::PasswordNeeded => continue,
            _ => return Err(attempt.message),
        }
    }
    Err("Too many incorrect configuration password attempts.".to_string())
}

fn verify_admin_password_for_client_config(
    parent: Option<&gtk::Window>,
    current_admin_hash: &str,
    incoming: &LoadedConfiguration,
) -> Result<(), String> {
    if current_admin_hash.is_empty() {
        return Ok(());
    }
    if incoming
        .settings
        .security
        .admin_password_hash
        .eq_ignore_ascii_case(current_admin_hash)
    {
        return Ok(());
    }
    if let Some(ref used) = incoming.used_password_hash {
        if used.eq_ignore_ascii_case(current_admin_hash) {
            return Ok(());
        }
    }
    for _ in 0..5 {
        let Some(entered) = dialogs::prompt_password(
            parent,
            "Administrator Password Required",
            "Enter the current SEB administrator password to replace the client configuration:",
        ) else {
            return Err("Administrator password entry cancelled.".to_string());
        };
        if verify_password_or_hash(&entered, current_admin_hash) {
            return Ok(());
        }
        dialogs::show_message(parent, "Invalid Password", "Incorrect administrator password.", true);
    }
    Err("Too many incorrect administrator password attempts.".to_string())
}

fn find_persistent_client_path(app_cfg: &seb_config::settings::AppConfig) -> Option<PathBuf> {
    if app_cfg.program_data_file_path.exists() {
        Some(app_cfg.program_data_file_path.clone())
    } else if app_cfg.app_data_file_path.exists() {
        Some(app_cfg.app_data_file_path.clone())
    } else {
        None
    }
}

fn run_server_workflow(
    cfg: &mut SessionConfiguration,
    splash: &RuntimeSplash,
    current_admin_hash: &str,
) -> Result<Option<ArcServer>, String> {
    let srv_cfg = cfg.settings.server.clone();
    let conn_browser_query = cfg.settings.browser.start_url_query.clone();

    loop {
        let result = (|| -> Result<SebServerClient, String> {
            splash.set_status("Connecting to SEB Server...");
            let mut client = SebServerClient::new(ServerConnectionConfig {
                server_url: srv_cfg.server_url.clone(),
                api_url: srv_cfg.api_url.clone(),
                client_name: srv_cfg.client_name.clone(),
                client_secret: srv_cfg.client_secret.clone(),
                institution: srv_cfg.institution.clone(),
                exam_id: srv_cfg.exam_id.clone(),
                request_attempts: srv_cfg.request_attempts,
                request_attempt_interval_ms: srv_cfg.request_attempt_interval_ms,
                timeout_ms: srv_cfg.request_timeout_ms,
            })?;
            client.discover_api()?;
            client.authenticate()?;

            splash.set_status("Retrieving available exams...");
            let hs = client.handshake("Linux", SEB_COMPATIBILITY_VERSION)?;
            let configured_exam_id = srv_cfg.exam_id.trim();
            let selected = if !configured_exam_id.is_empty() {
                hs.exams
                    .iter()
                    .find(|e| e.id == configured_exam_id)
                    .cloned()
                    .or_else(|| {
                        if hs.exams.len() == 1 {
                            hs.exams.first().cloned()
                        } else {
                            None
                        }
                    })
            } else {
                None
            }
            .or_else(|| dialogs::show_exam_selector(None, &hs.exams))
            .ok_or_else(|| "Exam selection cancelled.".to_string())?;

            splash.set_status("Downloading exam configuration...");
            let raw = client.fetch_exam_configuration(&selected.id)?;
            let loaded = load_server_exam_bytes(&raw, current_admin_hash)?;
            if !loaded.settings.browser.enable_browser {
                return Err("Exam configuration disables the browser.".to_string());
            }

            splash.set_status("Completing exam handshake...");
            let select_res = client.select_exam(&selected.id)?;

            let mut merged_settings = loaded.settings;
            if !selected.url.trim().is_empty() {
                merged_settings.browser.start_url = selected.url.clone();
            }
            if let Some(bek) = select_res.custom_browser_exam_key {
                merged_settings.browser.custom_browser_exam_key = bek;
            }
            merged_settings.browser.start_url_query = conn_browser_query.clone();
            // Preserve connection server settings while honoring exam invigilation flags
            let show_hand = merged_settings.server.show_raise_hand || srv_cfg.show_raise_hand;
            let force_msg =
                merged_settings.server.force_raise_hand_message || srv_cfg.force_raise_hand_message;
            merged_settings.server = srv_cfg.clone();
            merged_settings.server.show_raise_hand = show_hand;
            merged_settings.server.force_raise_hand_message = force_msg;
            merged_settings.session_mode = SessionMode::Server;

            cfg.settings = merged_settings;
            cfg.raw_seb_data = loaded.raw_seb_data;
            Ok(client)
        })();

        match result {
            Ok(client) => return Ok(Some(std::sync::Arc::new(std::sync::Mutex::new(client)))),
            Err(err) => match dialogs::show_server_failure(None, &err, srv_cfg.perform_fallback) {
                ServerFailureChoice::Retry => continue,
                ServerFailureChoice::Fallback => {
                    // Verify FallbackPasswordHash (5 attempts) and load persistent fallback config!
                    if !srv_cfg.fallback_password_hash.is_empty() {
                        let mut verified_pw = None;
                        for _ in 0..5 {
                            let Some(pw) = dialogs::prompt_password(
                                None,
                                "Fallback Password",
                                "Enter the password for the offline fallback configuration:",
                            ) else {
                                return Err("Fallback password entry cancelled.".to_string());
                            };
                            if verify_password_or_hash(&pw, &srv_cfg.fallback_password_hash) {
                                verified_pw = Some(pw);
                                break;
                            }
                            dialogs::show_message(
                                None,
                                "Invalid Password",
                                "Incorrect fallback password.",
                                true,
                            );
                        }
                        let Some(pw) = verified_pw else {
                            return Err("Too many incorrect fallback password attempts.".to_string());
                        };
                        if let Some(ppath) = find_persistent_client_path(&cfg.app_config) {
                            let loaded = load_from_resource(&ppath.to_string_lossy(), Some(&pw), false);
                            if loaded.status == LoadStatus::Success {
                                cfg.settings = loaded.settings;
                                cfg.raw_seb_data = loaded.raw_seb_data;
                            }
                        }
                    }
                    cfg.settings.session_mode = SessionMode::Normal;
                    return Ok(None);
                }
                ServerFailureChoice::Abort => return Err(err),
            },
        }
    }
}

fn load_server_exam_bytes(raw: &[u8], hint_hash: &str) -> Result<LoadedConfiguration, String> {
    let empty_hash = sha256_hex(b"");
    for (pw, is_hash) in [
        (Some(empty_hash.as_str()), true),
        (if hint_hash.is_empty() { None } else { Some(hint_hash) }, true),
    ] {
        let res = seb_config::loader::load_from_bytes(raw, pw, is_hash);
        match res.status {
            LoadStatus::Success => return Ok(res),
            LoadStatus::PasswordNeeded => continue,
            _ => return Err(res.message),
        }
    }
    for _ in 0..5 {
        let Some(pw) = dialogs::prompt_password(
            None,
            "Exam Configuration Password",
            "Enter the password for the SEB Server exam configuration:",
        ) else {
            return Err("Exam configuration password cancelled.".to_string());
        };
        let res = seb_config::loader::load_from_bytes(raw, Some(&pw), false);
        match res.status {
            LoadStatus::Success => return Ok(res),
            LoadStatus::PasswordNeeded => continue,
            _ => return Err(res.message),
        }
    }
    Err("Too many incorrect exam configuration password attempts.".to_string())
}

fn validate_runtime_security(
    cfg: &SessionConfiguration,
    integrity: &mut SessionIntegrityStore,
    linux_lockdown_active: bool,
) -> Result<(), String> {
    if cfg.is_browser_resource {
        return Ok(());
    }

    let sec = &cfg.settings.security;
    let svc = &cfg.settings.service;

    if !validate_version_restrictions(
        &sec.version_restrictions,
        SEB_COMPATIBILITY_BUILD_VERSION,
        false,
    ) {
        return Err(format!(
            "This configuration requires another Safe Exam Browser version.\nInstalled Linux SEB: {}\nSEB compatibility level: {}",
            cfg.app_config.program_build_version, cfg.app_config.seb_compatibility_build_version
        ));
    }

    if sec.verify_cursor_configuration {
        return Err("Cursor verification is not supported on Linux.".to_string());
    }
    if sec.disable_session_change_lock_screen {
        return Err("Disabling session-change lock screen is not supported on Linux.".to_string());
    }
    if !svc.ignore_service {
        match svc.policy {
            ServicePolicy::Mandatory if !linux_lockdown_active => {
                return Err("SEB Service policy is Mandatory and lockdown is not active.".to_string());
            }
            ServicePolicy::Optional if !linux_lockdown_active && svc.has_any_restricted_flag() => {
                return Err(
                    "Restricted Windows service policies require active Linux lockdown."
                        .to_string(),
                );
            }
            ServicePolicy::Warn => {
                dialogs::show_message(
                    None,
                    "Service Warning",
                    "This configuration expects the Windows SEB Service; Linux Polkit lockdown is used instead.",
                    false,
                );
            }
            _ => {}
        }
    }
    if sec.kiosk_mode != KioskMode::None && !linux_lockdown_active {
        return Err("Kiosk mode requires active Linux anti-cheat lockdown.".to_string());
    }
    if !sec.allow_window_capture && !linux_lockdown_active {
        return Err("Disabling window capture requires active Linux anti-cheat lockdown.".to_string());
    }

    if sec.virtual_machine_policy == VirtualMachinePolicy::Deny && is_virtual_machine() {
        return Err(
            "Virtual machine detected. Running this exam inside a VM is not allowed.".to_string(),
        );
    }

    if is_debugger_attached() {
        return Err("Debugger attachment detected. Safe Exam Browser cannot start.".to_string());
    }

    // Display monitor validation (uses /sys/class/drm connectors + GDK monitor fallback)
    validate_display(
        cfg.settings.display.allowed_displays,
        cfg.settings.display.internal_display_only,
        cfg.settings.display.ignore_error,
        &collect_gdk_screen_names(),
    )?;

    let verify = sec.verify_session_integrity;
    let has_quit_pw = !sec.quit_password_hash.is_empty();
    let key = &cfg.settings.browser.configuration_key;
    integrity.prepare(key, has_quit_pw, verify)?;
    integrity.activate(key, has_quit_pw, verify)?;
    Ok(())
}

fn collect_gdk_screen_names() -> Vec<String> {
    let mut names = Vec::new();
    if let Some(display) = gdk::Display::default() {
        let count = display.n_monitors();
        for i in 0..count {
            if let Some(mon) = display.monitor(i) {
                let model = mon
                    .model()
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                names.push(model);
            }
        }
    }
    names
}

fn apply_custom_proxy_to_wdm(
    wdm: &WebsiteDataManager,
    proxy: &seb_config::settings::ProxySettings,
) {
    let ignore_refs: Vec<&str> = proxy.bypass_list.iter().map(|s| s.as_str()).collect();
    let mut default_uri = None;
    for p in &proxy.proxies {
        if p.protocol == ProxyProtocol::Socks {
            default_uri = Some(format_proxy_uri("socks5", p));
        }
    }
    let mut settings = NetworkProxySettings::new(default_uri.as_deref(), &ignore_refs);
    for p in &proxy.proxies {
        match p.protocol {
            ProxyProtocol::Http => settings.add_proxy_for_scheme("http", &format_proxy_uri("http", p)),
            ProxyProtocol::Https => {
                settings.add_proxy_for_scheme("https", &format_proxy_uri("http", p))
            }
            ProxyProtocol::Ftp => settings.add_proxy_for_scheme("ftp", &format_proxy_uri("http", p)),
            ProxyProtocol::Socks => {}
        }
    }
    wdm.set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
}

fn format_proxy_uri(scheme: &str, p: &seb_config::settings::ProxyConfiguration) -> String {
    if p.requires_authentication && !p.username.is_empty() {
        format!("{scheme}://{}:{}@{}:{}", p.username, p.password, p.host.trim(), p.port)
    } else {
        format!("{scheme}://{}:{}", p.host.trim(), p.port)
    }
}

fn matches_reconfig_wildcard(url: &str, pattern: &str) -> bool {
    let candidates = equivalent_seb_urls(url);
    let escaped = regex_escape_wildcard(pattern);
    let Ok(re) = seb_filter::compile_wildcard_regex(&escaped) else {
        return candidates
            .iter()
            .any(|c| c.eq_ignore_ascii_case(pattern));
    };
    candidates.iter().any(|c| re.is_match(c))
}

fn equivalent_seb_urls(url: &str) -> Vec<String> {
    let mut out = vec![url.to_string()];
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("sebs://") {
        out.push(format!("https://{}", &url[7..]));
    } else if lower.starts_with("seb://") {
        out.push(format!("http://{}", &url[6..]));
        out.push(format!("https://{}", &url[6..]));
    } else if lower.starts_with("https://") {
        out.push(format!("sebs://{}", &url[8..]));
        out.push(format!("seb://{}", &url[8..]));
    } else if lower.starts_with("http://") {
        out.push(format!("seb://{}", &url[7..]));
    }
    out
}

fn regex_escape_wildcard(pattern: &str) -> String {
    let mut out = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    out
}

fn build_initial_start_url(settings: &AppSettings) -> String {
    let mut url = settings.browser.start_url.trim().to_string();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("sebs://") {
        url = format!("https://{}", &url[7..]);
    } else if lower.starts_with("seb://") {
        url = format!("http://{}", &url[6..]);
    }
    if settings.browser.use_query_parameter && !settings.browser.start_url_query.trim().is_empty() {
        let extra = settings
            .browser
            .start_url_query
            .trim()
            .trim_start_matches('?');
        if !extra.is_empty() {
            if let Ok(mut parsed) = Url::parse(&url) {
                let merged = match parsed.query() {
                    Some(existing) if !existing.is_empty() => format!("{existing}&{extra}"),
                    _ => extra.to_string(),
                };
                parsed.set_query(Some(&merged));
                url = parsed.to_string();
            } else if url.contains('?') {
                url.push('&');
                url.push_str(extra);
            } else {
                url.push('?');
                url.push_str(extra);
            }
        }
    }
    url
}

fn build_home_url(settings: &AppSettings) -> String {
    if settings.browser.use_start_url_as_home_url && settings.browser.home_url.trim().is_empty() {
        build_initial_start_url(settings)
    } else {
        let raw = settings.browser.home_url.trim();
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("sebs://") {
            format!("https://{}", &raw[7..])
        } else if lower.starts_with("seb://") {
            format!("http://{}", &raw[6..])
        } else {
            raw.to_string()
        }
    }
}

fn system_bcp47_locale() -> String {
    let raw = std::env::var("LC_MESSAGES")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "en-US".to_string());
    let base = raw.split('.').next().unwrap_or("en_US").split('@').next().unwrap_or("en_US");
    let tag = base.replace('_', "-");
    if tag.is_empty() || tag.eq_ignore_ascii_case("c") || tag.eq_ignore_ascii_case("posix") {
        "en-US".to_string()
    } else {
        tag
    }
}

fn sanitize_filename(raw: &str, fallback: &str) -> String {
    let base = Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(fallback)
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        fallback.to_string()
    } else {
        base.replace(['/', '\\', '\0'], "_")
    }
}

fn unique_path_in_dir(dir: &Path, filename: &str) -> PathBuf {
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    let p = Path::new(filename);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 1..=9999 {
        let candidate_name = if ext.is_empty() {
            format!("{stem}({n})")
        } else {
            format!("{stem}({n}).{ext}")
        };
        let cand = dir.join(candidate_name);
        if !cand.exists() {
            return cand;
        }
    }
    dir.join(format!("{stem}-{}", now_nanos()))
}

fn remove_cookie_files(dir: &Path) {
    for name in ["cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm"] {
        let _ = fs::remove_file(dir.join(name));
    }
}

fn clear_wdm_data_sync(wdm: &WebsiteDataManager, types: WebsiteDataTypes) {
    let done = Arc::new(AtomicBool::new(false));
    let done_cb = Arc::clone(&done);
    wdm.clear(
        types,
        glib::TimeSpan::from_seconds(0),
        gio::Cancellable::NONE,
        move |_| {
            done_cb.store(true, Ordering::Release);
        },
    );
    let deadline = Instant::now() + Duration::from_millis(800);
    while !done.load(Ordering::Acquire) && Instant::now() < deadline {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn build_blacklist(settings: &AppSettings) -> Vec<String> {
    let extras: Vec<String> = settings
        .applications
        .blacklist
        .iter()
        .map(|b| b.executable_name.clone())
        .collect();
    merge_prohibited_list(&extras)
}

fn compute_effective_bek(cfg: &SessionConfiguration) -> String {
    if !cfg.settings.browser.custom_browser_exam_key.trim().is_empty() {
        return cfg.settings.browser.custom_browser_exam_key.trim().to_string();
    }
    if cfg.settings.browser.configuration_key.is_empty() {
        return String::new();
    }
    compute_browser_exam_key(
        &cfg.app_config.code_signature_hash,
        &cfg.app_config.program_build_version,
        &cfg.settings.browser.configuration_key,
        &cfg.settings.browser.exam_key_salt,
    )
}

fn build_user_agent(settings: &AppSettings) -> String {
    let base = if settings.browser.use_custom_user_agent
        && !settings.browser.custom_user_agent.trim().is_empty()
    {
        settings.browser.custom_user_agent.trim().to_string()
    } else {
        // Keep Version/17.0 instead of WebKitGTK's raw `Settings::new().user_agent()` (`Version/60.5`),
        // because Microsoft Entra SSO rejects/blocks cookies when `Version/60.5 Safari/605.1.15` is sent.
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15".to_string()
    };
    let mut ua = format!(
        "{base} SEB-Linux/{SEB_APPLICATION_VERSION} SEB/{SEB_COMPATIBILITY_VERSION}"
    );
    if !settings.browser.user_agent_suffix.trim().is_empty() {
        ua.push(' ');
        ua.push_str(settings.browser.user_agent_suffix.trim());
    }
    ua
}

fn resolve_web_extension_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("libseb_web_extension.so").is_file() {
                return Some(dir.to_path_buf());
            }
            if let Some(parent) = dir.parent() {
                let rel = parent.join("release");
                if rel.join("libseb_web_extension.so").is_file() {
                    return Some(rel);
                }
            }
        }
    }
    for sys in [
        "/usr/lib/safe-exam-browser",
        "/usr/libexec/safe-exam-browser",
        "/usr/lib64/safe-exam-browser",
    ] {
        let p = PathBuf::from(sys);
        if p.join("libseb_web_extension.so").is_file() {
            return Some(p);
        }
    }
    None
}

impl Drop for SebAppShell {
    fn drop(&mut self) {
        self.power_inhibitor.borrow_mut().stop();
        self.lockdown.borrow_mut().stop();
    }
}

fn parse_edx_user_info_cookie(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"');
    // Unescape common cookie quoting (\054 -> , and \" -> ")
    let unescaped = trimmed.replace("\\054", ",").replace("\\\"", "\"");
    let v: serde_json::Value = serde_json::from_str(&unescaped).ok()?;
    v.get("username")
        .and_then(|u| u.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_pdf_to_inline_html(
    pdf_url: &str,
    cookie_hdr: &str,
    tmp_dir: &Path,
    pdf_path: &Path,
    show_toolbar: bool,
    allow_print: bool,
) -> Result<String, String> {
    struct TempDirCleanup(PathBuf);
    impl Drop for TempDirCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = TempDirCleanup(tmp_dir.to_path_buf());

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let mut req = agent.get(pdf_url);
    if !cookie_hdr.is_empty() {
        req = req.set("Cookie", cookie_hdr);
    }
    let resp = req
        .call()
        .map_err(|e| format!("Failed to fetch PDF document: {e}"))?;
    let mut bytes = Vec::new();
    use std::io::Read as _;
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read PDF document: {e}"))?;
    fs::write(pdf_path, &bytes).map_err(|e| format!("Failed to store temporary PDF: {e}"))?;

    let prefix = tmp_dir.join("page");
    let status = std::process::Command::new("pdftoppm")
        .args(["-png", "-r", "130"])
        .arg(pdf_path)
        .arg(&prefix)
        .status()
        .map_err(|_| {
            "Inline PDF rendering requires 'pdftoppm' (poppler-utils), which is not installed."
                .to_string()
        })?;
    if !status.success() {
        return Err("Failed to render PDF pages with pdftoppm.".to_string());
    }

    let mut pages: Vec<PathBuf> = fs::read_dir(tmp_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("page-") && n.ends_with(".png"))
        })
        .collect();
    pages.sort();
    if pages.is_empty() {
        return Err("PDF document contained no renderable pages.".to_string());
    }

    let mut imgs = String::new();
    for (idx, p) in pages.iter().enumerate() {
        if let Ok(png) = fs::read(p) {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
            imgs.push_str(&format!(
                "<div class=\"pg\" id=\"p{}\"><img src=\"data:image/png;base64,{}\"/></div>",
                idx + 1,
                b64
            ));
        }
    }
    let _ = fs::remove_dir_all(tmp_dir);

    let toolbar = if show_toolbar {
        let print_btn = if allow_print {
            "<button onclick=\"window.print()\">Print</button>"
        } else {
            ""
        };
        format!(
            "<div class=\"tb\"><span>PDF Viewer ({} pages)</span><div><button onclick=\"z(0.85)\">-</button><button onclick=\"z(1.18)\">+</button>{print_btn}</div></div>",
            pages.len()
        )
    } else {
        String::new()
    };

    Ok(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>body{{margin:0;background:#d8d8d8;font-family:sans-serif;}}.tb{{position:sticky;top:0;background:#f0f0f0;border-bottom:1px solid #b0b0b0;padding:6px 12px;display:flex;justify-content:space-between;align-items:center;}}button{{background:#f0f0f0;border:1px solid gray;padding:3px 10px;margin-left:6px;cursor:pointer;}}.pg{{text-align:center;margin:12px auto;}}img{{width:94%;box-shadow:0 1px 4px rgba(0,0,0,0.25);background:#fff;}}</style><script>var s=1;function z(f){{s=Math.max(0.4,Math.min(3,s*f));document.querySelectorAll('img').forEach(i=>i.style.width=(s*94)+'%');}}</script></head><body>{toolbar}{imgs}</body></html>"
    ))
}

fn flush_proctoring_with_dialog(parent: &gtk::Window, client: &ScreenProctoringClient) {
    let initial = client.pending_count();
    if initial == 0 {
        return;
    }
    let dlg = gtk::Dialog::new();
    dlg.set_transient_for(Some(parent));
    dlg.set_modal(true);
    dlg.set_title("Screen Proctoring");
    dlg.set_default_size(400, 140);
    dlg.style_context().add_class("seb-dialog");
    dialogs::make_window_frameless(&dlg);

    let content = dlg.content_area();
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(14);
    vbox.set_margin_start(18);
    vbox.set_margin_end(18);

    let lbl = gtk::Label::new(Some(&format!(
        "Uploading remaining Screen Proctoring data ({initial} queued)..."
    )));
    lbl.set_xalign(0.0);
    let pb = gtk::ProgressBar::new();
    pb.set_fraction(0.0);
    let force_btn = gtk::Button::with_label("Force Quit");
    force_btn.set_halign(gtk::Align::End);
    let force_quit = Rc::new(Cell::new(false));
    {
        let fq = Rc::clone(&force_quit);
        force_btn.connect_clicked(move |_| fq.set(true));
    }

    vbox.pack_start(&lbl, false, false, 0);
    vbox.pack_start(&pb, false, false, 0);
    vbox.pack_start(&force_btn, false, false, 4);
    content.pack_start(&vbox, true, true, 0);
    dlg.show_all();

    let deadline = Instant::now() + Duration::from_secs(15);
    while !force_quit.get() && Instant::now() < deadline {
        let rem = client.pending_count();
        if rem == 0 {
            break;
        }
        let done = initial.saturating_sub(rem);
        pb.set_fraction((done as f64 / initial.max(1) as f64).clamp(0.0, 1.0));
        lbl.set_text(&format!(
            "Uploading remaining Screen Proctoring data ({rem} remaining)..."
        ));
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
    dlg.close();
}
