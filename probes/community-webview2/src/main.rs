#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod host;

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if !arguments.is_empty() {
        if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
            println!("Veylurk standalone Community WebView2 proof\n\nRun without arguments to open normal Twitch login in the existing runtime.\nThe owner signs in and starts each sample using native app buttons.\nNo downloads, installations, private request replay, or automatic collection.\nOutput: aggregate-evidence.json and data/browser-profile next to the executable.\n--help exits without COM, a window, or network access.");
        } else {
            eprintln!("Unsupported arguments. Use --help or run without arguments.");
        }
        return;
    }
    #[cfg(windows)]
    host::run();
    #[cfg(not(windows))]
    eprintln!("The native host requires Windows. Offline parser tests are portable.");
}
