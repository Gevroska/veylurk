#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod host;

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
        println!("Veylurk standalone Community WebView2 proof\n\nNo arguments: owner login and manual samples.\n--autonomous --channel LOGIN --channel LOGIN [--channel LOGIN] [--circuits 1..3] [--max-seconds 60..600]\nAutonomous mode uses normal full channel pages in the existing app profile, verifies the normal player pause, then clicks Community. Default: two circuits, 240 seconds; starts at least 30 seconds apart.\nNo popout fallback, installations, private HTTP, request replay, or imported credentials.\nOutput: aggregate-evidence.json and data/browser-profile next to the executable.\n--help exits before COM, a window, or network access.");
        return;
    }
    let config = match community_webview2_proof::sampling::arguments(&arguments) {
        Ok(config) => config,
        Err(code) => {
            eprintln!("Invalid proof arguments: {code}. Use --help.");
            std::process::exit(2);
        }
    };
    #[cfg(windows)]
    host::run(config);
    #[cfg(not(windows))]
    {
        let _ = config;
        eprintln!("The native host requires Windows. Offline parser tests are portable.");
    }
}
