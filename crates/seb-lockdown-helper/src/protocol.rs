use serde::{Deserialize, Serialize};

pub const VIRTUAL_KEYBOARD_NAME: &str = "SEB Virtual Keyboard";
pub const HEARTBEAT_INTERVAL_MS: u64 = 2000;
pub const WATCHDOG_TIMEOUT_MS: u64 = 6000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardLockPolicy {
    #[serde(default)]
    pub allow_alt_tab: bool,
    #[serde(default)]
    pub allow_alt_f4: bool,
    #[serde(default)]
    pub allow_alt_esc: bool,
    #[serde(default)]
    pub allow_ctrl_esc: bool,
    #[serde(default)]
    pub allow_print_screen: bool,
    #[serde(default)]
    pub allow_system_key: bool,
    #[serde(default = "default_true")]
    pub allow_action_center_combo: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingCommand {
    pub cmd: String,
    #[serde(default)]
    pub seb_pid: Option<i64>,
    #[serde(default)]
    pub blacklist: Option<Vec<String>>,
    #[serde(default)]
    pub keyboard: Option<KeyboardLockPolicy>,
}
