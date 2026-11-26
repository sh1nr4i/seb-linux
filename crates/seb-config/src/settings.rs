use crate::sha256_hex;
use seb_filter::FilterRuleConfig;
use seb_security::VersionRestriction;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SEB_APPLICATION_VERSION: &str = "0.6.3";
pub const SEB_COMPATIBILITY_VERSION: &str = "3.10.2";
pub const SEB_COMPATIBILITY_BUILD_VERSION: &str = "3.10.2.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConfigurationMode {
    #[default]
    Exam,
    ConfigureClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SessionMode {
    #[default]
    Normal,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LogLevel {
    #[default]
    Debug,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PopupPolicy {
    Block,
    AllowSameWindow,
    #[default]
    Allow,
    AllowSameHost,
    AllowSameHostAndWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UrlPolicy {
    #[default]
    Never,
    LoadError,
    BeforeTitle,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WindowPosition {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClipboardPolicy {
    #[default]
    Isolated,
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KioskMode {
    #[default]
    None,
    CreateNewDesktop,
    DisableExplorerShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VirtualMachinePolicy {
    #[default]
    Deny,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UserInterfaceMode {
    #[default]
    Desktop,
    Mobile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UserAgentMode {
    #[default]
    Default,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProctoringImageFormat {
    Bmp,
    Gif,
    Jpg,
    #[default]
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProctoringImageQuantization {
    BlackAndWhite1bpp,
    Grayscale2bpp,
    #[default]
    Grayscale4bpp,
    Grayscale8bpp,
    Color8bpp,
    Color16bpp,
    Color24bpp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProxyPolicy {
    #[default]
    System,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyProtocol {
    Ftp,
    Http,
    Https,
    Socks,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfiguration {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub requires_authentication: bool,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxySettings {
    pub policy: ProxyPolicy,
    pub auto_configure: bool,
    pub auto_configure_url: String,
    pub auto_detect: bool,
    pub bypass_list: Vec<String>,
    pub proxies: Vec<ProxyConfiguration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ServicePolicy {
    #[default]
    Optional,
    Warn,
    Mandatory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSettings {
    pub ignore_service: bool,
    pub policy: ServicePolicy,
    pub disable_chrome_notifications: bool,
    pub disable_ease_of_access_options: bool,
    pub disable_find_printer: bool,
    pub disable_network_options: bool,
    pub disable_password_change: bool,
    pub disable_power_options: bool,
    pub disable_remote_connections: bool,
    pub disable_signout: bool,
    pub disable_task_manager: bool,
    pub disable_user_lock: bool,
    pub disable_user_switch: bool,
    pub disable_vmware_overlay: bool,
    pub disable_windows_update: bool,
    pub set_vmware_configuration: bool,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            ignore_service: true,
            policy: ServicePolicy::Optional,
            disable_chrome_notifications: false,
            disable_ease_of_access_options: false,
            disable_find_printer: false,
            disable_network_options: false,
            disable_password_change: false,
            disable_power_options: false,
            disable_remote_connections: false,
            disable_signout: false,
            disable_task_manager: false,
            disable_user_lock: false,
            disable_user_switch: false,
            disable_vmware_overlay: false,
            disable_windows_update: false,
            set_vmware_configuration: false,
        }
    }
}

impl ServiceSettings {
    pub fn has_any_restricted_flag(&self) -> bool {
        self.disable_chrome_notifications
            || self.disable_ease_of_access_options
            || self.disable_find_printer
            || self.disable_network_options
            || self.disable_password_change
            || self.disable_power_options
            || self.disable_remote_connections
            || self.disable_signout
            || self.disable_task_manager
            || self.disable_user_lock
            || self.disable_user_switch
            || self.disable_vmware_overlay
            || self.disable_windows_update
            || self.set_vmware_configuration
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerSupplySettings {
    pub charge_threshold_critical: f64,
    pub charge_threshold_low: f64,
}

impl Default for PowerSupplySettings {
    fn default() -> Self {
        Self {
            charge_threshold_critical: 0.1,
            charge_threshold_low: 0.2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSettings {
    pub allow_address_bar: bool,
    pub allow_backward_navigation: bool,
    pub allow_developer_console: bool,
    pub allow_forward_navigation: bool,
    pub allow_minimize: bool,
    pub allow_reloading: bool,
    pub always_on_top: bool,
    pub full_screen_mode: bool,
    pub position: WindowPosition,
    pub relative_height: Option<i32>,
    pub relative_width: Option<i32>,
    pub absolute_height: Option<i32>,
    pub absolute_width: Option<i32>,
    pub show_home_button: bool,
    pub show_reload_button: bool,
    pub show_reload_warning: bool,
    pub show_toolbar: bool,
    pub url_policy: UrlPolicy,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            allow_address_bar: false,
            allow_backward_navigation: false,
            allow_developer_console: false,
            allow_forward_navigation: false,
            allow_minimize: true,
            allow_reloading: true,
            always_on_top: false,
            full_screen_mode: false,
            position: WindowPosition::Left,
            relative_height: Some(100),
            relative_width: Some(100),
            absolute_height: None,
            absolute_width: None,
            show_home_button: false,
            show_reload_button: true,
            show_reload_warning: false,
            show_toolbar: false,
            url_policy: UrlPolicy::Never,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilterSettings {
    pub process_content_requests: bool,
    pub process_main_requests: bool,
    pub rules: Vec<FilterRuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSettings {
    pub additional_window: WindowSettings,
    pub allow_configuration_downloads: bool,
    pub allow_custom_down_upload_location: bool,
    pub allow_downloads: bool,
    pub allow_find: bool,
    pub allow_page_zoom: bool,
    pub allow_pdf_reader: bool,
    pub allow_pdf_reader_toolbar: bool,
    pub allow_print: bool,
    pub allow_spell_checking: bool,
    pub allow_uploads: bool,
    pub configuration_key: String,
    pub custom_browser_exam_key: String,
    pub custom_user_agent: String,
    pub delete_cache_on_shutdown: bool,
    pub delete_cookies_on_shutdown: bool,
    pub delete_cookies_on_startup: bool,
    pub down_upload_directory: String,
    pub enable_browser: bool,
    pub exam_key_salt: Vec<u8>,
    pub filter: FilterSettings,
    pub home_navigation_message: String,
    pub home_navigation_requires_password: bool,
    pub home_password_hash: String,
    pub home_url: String,
    pub main_window: WindowSettings,
    pub media_capture_camera: bool,
    pub media_capture_microphone: bool,
    pub popup_policy: PopupPolicy,
    pub proxy: ProxySettings,
    pub confirm_quit_url: bool,
    pub quit_url: String,
    pub reset_on_quit_url: bool,
    pub send_browser_exam_key: bool,
    pub send_configuration_key: bool,
    pub show_file_system_element_path: bool,
    pub start_url: String,
    pub start_url_query: String,
    pub use_custom_user_agent: bool,
    pub use_isolated_clipboard: bool,
    pub use_query_parameter: bool,
    pub use_start_url_as_home_url: bool,
    pub use_temporary_down_upload_directory: bool,
    pub user_agent_mode: UserAgentMode,
    pub user_agent_suffix: String,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        let mut main_window = WindowSettings::default();
        main_window.allow_minimize = false;
        main_window.always_on_top = true;
        main_window.show_reload_warning = true;

        let mut additional_window = WindowSettings::default();
        additional_window.relative_width = Some(50);
        additional_window.position = WindowPosition::Right;
        additional_window.allow_backward_navigation = true;
        additional_window.allow_forward_navigation = true;

        Self {
            additional_window,
            allow_configuration_downloads: true,
            allow_custom_down_upload_location: false,
            allow_downloads: true,
            allow_find: true,
            allow_page_zoom: true,
            allow_pdf_reader: true,
            allow_pdf_reader_toolbar: false,
            allow_print: false,
            allow_spell_checking: false,
            allow_uploads: false,
            configuration_key: String::new(),
            custom_browser_exam_key: String::new(),
            custom_user_agent: String::new(),
            delete_cache_on_shutdown: true,
            delete_cookies_on_shutdown: true,
            delete_cookies_on_startup: true,
            down_upload_directory: String::new(),
            enable_browser: true,
            exam_key_salt: Vec::new(),
            filter: FilterSettings::default(),
            home_navigation_message: String::new(),
            home_navigation_requires_password: false,
            home_password_hash: String::new(),
            home_url: String::new(),
            main_window,
            media_capture_camera: false,
            media_capture_microphone: false,
            popup_policy: PopupPolicy::Allow,
            proxy: ProxySettings::default(),
            confirm_quit_url: false,
            quit_url: String::new(),
            reset_on_quit_url: false,
            send_browser_exam_key: false,
            send_configuration_key: false,
            show_file_system_element_path: true,
            start_url: "https://www.safeexambrowser.org/start".to_string(),
            start_url_query: String::new(),
            use_custom_user_agent: false,
            use_isolated_clipboard: true,
            use_query_parameter: false,
            use_start_url_as_home_url: false,
            use_temporary_down_upload_directory: false,
            user_agent_mode: UserAgentMode::Default,
            user_agent_suffix: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlacklistApplication {
    pub auto_terminate: bool,
    pub executable_name: String,
    pub original_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WhitelistApplication {
    pub allow_custom_path: bool,
    pub allow_running: bool,
    pub arguments: Vec<String>,
    pub auto_start: bool,
    pub auto_terminate: bool,
    pub description: String,
    pub display_name: String,
    pub executable_name: String,
    pub executable_path: String,
    pub original_name: String,
    pub show_in_shell: bool,
    pub signature: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplicationSettings {
    pub blacklist: Vec<BlacklistApplication>,
    pub whitelist: Vec<WhitelistApplication>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioSettings {
    pub initial_volume: i32,
    pub initialize_volume: bool,
    pub mute_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplaySettings {
    pub allowed_displays: i32,
    pub always_on: bool,
    pub ignore_error: bool,
    pub internal_display_only: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            allowed_displays: 1,
            always_on: true,
            ignore_error: false,
            internal_display_only: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyboardSettings {
    pub allow_alt_esc: bool,
    pub allow_alt_f4: bool,
    pub allow_alt_tab: bool,
    pub allow_ctrl_c: bool,
    pub allow_ctrl_esc: bool,
    pub allow_ctrl_v: bool,
    pub allow_ctrl_x: bool,
    pub allow_esc: bool,
    pub allow_f1: bool,
    pub allow_f2: bool,
    pub allow_f3: bool,
    pub allow_f4: bool,
    pub allow_f5: bool,
    pub allow_f6: bool,
    pub allow_f7: bool,
    pub allow_f8: bool,
    pub allow_f9: bool,
    pub allow_f10: bool,
    pub allow_f11: bool,
    pub allow_f12: bool,
    pub allow_injected: bool,
    pub allow_print_screen: bool,
    pub allow_system_key: bool,
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            allow_alt_esc: false,
            allow_alt_f4: false,
            allow_alt_tab: true,
            allow_ctrl_c: true,
            allow_ctrl_esc: false,
            allow_ctrl_v: true,
            allow_ctrl_x: true,
            allow_esc: true,
            allow_f1: true,
            allow_f2: true,
            allow_f3: true,
            allow_f4: true,
            allow_f5: true,
            allow_f6: true,
            allow_f7: true,
            allow_f8: true,
            allow_f9: true,
            allow_f10: true,
            allow_f11: true,
            allow_f12: true,
            allow_injected: false,
            allow_print_screen: false,
            allow_system_key: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseSettings {
    pub allow_middle_button: bool,
    pub allow_right_button: bool,
}

impl Default for MouseSettings {
    fn default() -> Self {
        Self {
            allow_middle_button: false,
            allow_right_button: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProctoringMetaDataSettings {
    pub capture_application_data: bool,
    pub capture_browser_data: bool,
    pub capture_window_title: bool,
}

impl Default for ProctoringMetaDataSettings {
    fn default() -> Self {
        Self {
            capture_application_data: true,
            capture_browser_data: true,
            capture_window_title: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenProctoringSettings {
    pub cache_size: usize,
    pub client_id: String,
    pub client_secret: String,
    pub enabled: bool,
    pub encryption_secret: String,
    pub group_id: String,
    pub image_format: ProctoringImageFormat,
    pub image_downscaling: f64,
    pub interval_maximum: u64,
    pub interval_minimum: u64,
    pub meta_data: ProctoringMetaDataSettings,
    pub image_quantization: ProctoringImageQuantization,
    pub service_url: String,
}

impl Default for ScreenProctoringSettings {
    fn default() -> Self {
        Self {
            cache_size: 500,
            client_id: String::new(),
            client_secret: String::new(),
            enabled: false,
            encryption_secret: String::new(),
            group_id: String::new(),
            image_format: ProctoringImageFormat::Png,
            image_downscaling: 1.0,
            interval_maximum: 5000,
            interval_minimum: 1000,
            meta_data: ProctoringMetaDataSettings::default(),
            image_quantization: ProctoringImageQuantization::Grayscale4bpp,
            service_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProctoringSettings {
    pub enabled: bool,
    pub show_disclaimer: bool,
    pub show_taskbar_notification: bool,
    pub allow_chat: bool,
    pub receive_audio: bool,
    pub receive_video: bool,
    pub screen_proctoring: ScreenProctoringSettings,
}

impl Default for ProctoringSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            show_disclaimer: true,
            show_taskbar_notification: true,
            allow_chat: false,
            receive_audio: false,
            receive_video: false,
            screen_proctoring: ScreenProctoringSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecuritySettings {
    pub admin_password_hash: String,
    pub allow_application_log: bool,
    pub allow_reconfiguration: bool,
    pub allow_sticky_keys: bool,
    pub allow_termination: bool,
    pub allow_window_capture: bool,
    pub clipboard_policy: ClipboardPolicy,
    pub disable_session_change_lock_screen: bool,
    pub kiosk_mode: KioskMode,
    pub quit_password_hash: String,
    pub reconfiguration_url: String,
    pub verify_cursor_configuration: bool,
    pub verify_session_integrity: bool,
    pub version_restrictions: Vec<VersionRestriction>,
    pub virtual_machine_policy: VirtualMachinePolicy,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            admin_password_hash: String::new(),
            allow_application_log: false,
            allow_reconfiguration: true,
            allow_sticky_keys: false,
            allow_termination: true,
            allow_window_capture: true,
            clipboard_policy: ClipboardPolicy::Isolated,
            disable_session_change_lock_screen: false,
            kiosk_mode: KioskMode::None,
            quit_password_hash: String::new(),
            reconfiguration_url: String::new(),
            verify_cursor_configuration: false,
            verify_session_integrity: true,
            version_restrictions: Vec::new(),
            virtual_machine_policy: VirtualMachinePolicy::Deny,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSettings {
    pub api_url: String,
    pub client_name: String,
    pub client_secret: String,
    pub exam_id: String,
    pub fallback_password_hash: String,
    pub institution: String,
    pub perform_fallback: bool,
    pub ping_interval_ms: u64,
    pub request_attempts: u32,
    pub request_attempt_interval_ms: u64,
    pub request_timeout_ms: u64,
    pub server_url: String,
    pub show_raise_hand: bool,
    pub force_raise_hand_message: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            api_url: "/sebapi/v1/api".to_string(),
            client_name: String::new(),
            client_secret: String::new(),
            exam_id: String::new(),
            fallback_password_hash: String::new(),
            institution: String::new(),
            perform_fallback: false,
            ping_interval_ms: 1000,
            request_attempts: 5,
            request_attempt_interval_ms: 2000,
            request_timeout_ms: 30000,
            server_url: String::new(),
            show_raise_hand: true,
            force_raise_hand_message: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionCenterSettings {
    pub enable_action_center: bool,
    pub show_application_info: bool,
    pub show_application_log: bool,
    pub show_audio: bool,
    pub show_clock: bool,
    pub show_keyboard_layout: bool,
    pub show_network: bool,
}

impl Default for ActionCenterSettings {
    fn default() -> Self {
        Self {
            enable_action_center: true,
            show_application_info: true,
            show_application_log: false,
            show_audio: false,
            show_clock: true,
            show_keyboard_layout: true,
            show_network: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskbarSettings {
    pub enable_taskbar: bool,
    pub show_application_info: bool,
    pub show_application_log: bool,
    pub show_audio: bool,
    pub show_clock: bool,
    pub show_keyboard_layout: bool,
    pub show_network: bool,
    pub show_verificator: bool,
}

impl Default for TaskbarSettings {
    fn default() -> Self {
        Self {
            enable_taskbar: true,
            show_application_info: false,
            show_application_log: false,
            show_audio: false,
            show_clock: true,
            show_keyboard_layout: true,
            show_network: false,
            show_verificator: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockScreenSettings {
    pub background_color: String,
}

impl Default for LockScreenSettings {
    fn default() -> Self {
        Self {
            background_color: "#ff0000".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInterfaceSettings {
    pub action_center: ActionCenterSettings,
    pub lock_screen: LockScreenSettings,
    pub mode: UserInterfaceMode,
    pub taskbar: TaskbarSettings,
}

impl Default for UserInterfaceSettings {
    fn default() -> Self {
        Self {
            action_center: ActionCenterSettings::default(),
            lock_screen: LockScreenSettings::default(),
            mode: UserInterfaceMode::Desktop,
            taskbar: TaskbarSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub applications: ApplicationSettings,
    pub audio: AudioSettings,
    pub browser: BrowserSettings,
    pub configuration_mode: ConfigurationMode,
    pub keep_client_config_encryption: bool,
    pub display: DisplaySettings,
    pub keyboard: KeyboardSettings,
    pub log_level: LogLevel,
    pub mouse: MouseSettings,
    pub power_supply: PowerSupplySettings,
    pub proctoring: ProctoringSettings,
    pub security: SecuritySettings,
    pub server: ServerSettings,
    pub service: ServiceSettings,
    pub session_mode: SessionMode,
    pub system_always_on: bool,
    pub user_interface: UserInterfaceSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            applications: ApplicationSettings::default(),
            audio: AudioSettings::default(),
            browser: BrowserSettings::default(),
            configuration_mode: ConfigurationMode::Exam,
            keep_client_config_encryption: false,
            display: DisplaySettings::default(),
            keyboard: KeyboardSettings::default(),
            log_level: LogLevel::Debug,
            mouse: MouseSettings::default(),
            power_supply: PowerSupplySettings::default(),
            proctoring: ProctoringSettings::default(),
            security: SecuritySettings::default(),
            server: ServerSettings::default(),
            service: ServiceSettings::default(),
            session_mode: SessionMode::Normal,
            system_always_on: true,
            user_interface: UserInterfaceSettings::default(),
        }
    }
}

fn calculate_executable_hash() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return String::new();
    };
    let Ok(bytes) = std::fs::read(exe) else {
        return String::new();
    };
    sha256_hex(&bytes)
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub app_data_file_path: PathBuf,
    pub program_data_file_path: PathBuf,
    pub browser_profile_path: PathBuf,
    pub browser_cache_path: PathBuf,
    pub session_cache_file_path: PathBuf,
    pub temporary_directory: PathBuf,
    pub code_signature_hash: String,
    pub program_build_version: String,
    pub seb_compatibility_version: String,
    pub seb_compatibility_build_version: String,
}

impl AppConfig {
    pub fn initialize() -> Self {
        let data_dir = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Safe Exam Browser");
        let cache_dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("Safe Exam Browser");

        Self {
            app_data_file_path: data_dir.join("client.seb"),
            program_data_file_path: PathBuf::from("/etc/safe-exam-browser/client.seb"),
            browser_profile_path: data_dir.join("profile"),
            browser_cache_path: cache_dir.join("cache"),
            session_cache_file_path: data_dir.join("Temp/cache.bin"),
            temporary_directory: data_dir.join("Temp"),
            code_signature_hash: calculate_executable_hash(),
            program_build_version: SEB_APPLICATION_VERSION.to_string(),
            seb_compatibility_version: SEB_COMPATIBILITY_VERSION.to_string(),
            seb_compatibility_build_version: SEB_COMPATIBILITY_BUILD_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfiguration {
    pub app_config: AppConfig,
    pub settings: AppSettings,
    pub is_client_configuration: bool,
    pub is_browser_resource: bool,
    pub raw_seb_data: Option<Vec<u8>>,
}

impl SessionConfiguration {
    pub fn default_session() -> Self {
        Self {
            app_config: AppConfig::initialize(),
            settings: AppSettings::default(),
            is_client_configuration: true,
            is_browser_resource: false,
            raw_seb_data: None,
        }
    }
}

pub fn make_filter_rule(
    expression: String,
    allow: bool,
    is_regex: bool,
) -> FilterRuleConfig {
    FilterRuleConfig {
        active: true,
        expression,
        is_regex,
        allow,
    }
}
