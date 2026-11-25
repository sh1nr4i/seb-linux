mod applications;
mod assets;
mod browser_pane;
mod clipboard;
mod dialogs;
mod overlays;
mod shell;
mod style;
mod taskbar;

use seb_config::settings::{SEB_APPLICATION_VERSION, SEB_COMPATIBILITY_VERSION};
use shell::{LaunchOptions, SebAppShell};
use std::process::ExitCode;

fn print_help() {
    println!("Safe Exam Browser for Linux {SEB_APPLICATION_VERSION} (SEB/{SEB_COMPATIBILITY_VERSION}) — 100% Pure Rust");
    println!();
    println!("Usage: safe-exam-browser [OPTIONS] [resource]");
    println!();
    println!("Options:");
    println!("  -h, --help            Displays help on commandline options.");
    println!("  -v, --version         Displays version information.");
    println!("  -c, --config <file>   Load settings from a JSON file or .seb configuration.");
    #[cfg(feature = "dev")]
    {
        println!("  --dev-bypass          Skip strict lockdowns and environment checks (Dev Build only).");
        println!("  --anti-cheat          Enable strict superuser anti-cheat lockdown (always enforced in User Build).");
    }
    println!();
    println!("Arguments:");
    println!("  resource              Open a local .seb file, remote .seb URL, or seb:// / sebs:// resource.");
}

fn validate_environment() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("Safe Exam Browser must run as an unprivileged user.");
        return false;
    }
    true
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut resource: Option<String> = None;
    #[allow(unused_mut)]
    let mut dev_bypass = false;
    #[cfg(feature = "dev")]
    let mut anti_cheat_flag = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            "-v" | "--version" => {
                println!("safe-exam-browser {SEB_APPLICATION_VERSION} (SEB/{SEB_COMPATIBILITY_VERSION})");
                return ExitCode::SUCCESS;
            }
            "-c" | "--config" => {
                if i + 1 < args.len() {
                    resource = Some(args[i + 1].clone());
                    i += 2;
                    continue;
                } else {
                    eprintln!("error: --config requires a file or URL argument.");
                    return ExitCode::FAILURE;
                }
            }
            #[cfg(feature = "dev")]
            "--dev-bypass" => {
                dev_bypass = true;
            }
            #[cfg(feature = "dev")]
            "--anti-cheat" => {
                anti_cheat_flag = true;
            }
            other if other.starts_with('-') => {
                eprintln!("Unknown option: {other}");
                return ExitCode::FAILURE;
            }
            pos => {
                if resource.is_none() {
                    resource = Some(pos.to_string());
                }
            }
        }
        i += 1;
    }

    if !dev_bypass && !validate_environment() {
        return ExitCode::FAILURE;
    }

    #[cfg(not(feature = "dev"))]
    {
        // Prevent non-root debuggers/tracers from attaching via ptrace in User Build (AGENTS.md).
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        }
    }

    // Prevent libsoup-3.0 inside WebKitNetworkProcess from attempting Kerberos/SPNEGO Negotiate
    // auth on SSO endpoints (e.g. Microsoft Entra) and emitting gssapi-WARNING without tickets.
    if std::env::var_os("SOUP_GSSAPI_BLOCKLISTED_URIS").is_none() {
        std::env::set_var("SOUP_GSSAPI_BLOCKLISTED_URIS", "https://,http://");
    }

    if gtk::init().is_err() {
        eprintln!("Failed to initialize GTK3 display.");
        return ExitCode::FAILURE;
    }

    style::install_light_theme();

    #[cfg(feature = "dev")]
    let strict_lockdown = anti_cheat_flag && !dev_bypass;
    #[cfg(not(feature = "dev"))]
    let strict_lockdown = true;

    let opts = LaunchOptions {
        resource,
        dev_bypass,
        strict_lockdown,
    };

    match SebAppShell::launch(opts) {
        Ok(_shell) => {
            gtk::main();
            ExitCode::SUCCESS
        }
        Err(msg) => {
            if msg == "Client configuration saved successfully." {
                ExitCode::SUCCESS
            } else {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
        }
    }
}
