//! `framekeep-trayd` -- the IPC server with no window on top.
//!
//! What CI runs and what the MCP adapter can be tested against. The app proper
//! is `framekeep-tray` (feature `gui`); both go through [`framekeep_tray::bring_up`],
//! so neither can drift into a startup sequence the other forgot.

use std::io::Write;

const USAGE: &str = "\
framekeep-trayd -- Framekeep's IPC server, without the app window

USAGE:
  framekeep-trayd [OPTIONS]

OPTIONS:
  --address <name>   Listen somewhere other than the default
  --print-address    Print the address this machine would use, then exit
  -h, --help         Show this help
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut address: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--print-address" => match framekeep_tray::transport::default_address() {
                Ok(a) => {
                    println!("{a}");
                    return;
                }
                Err(e) => fail(&format!("Couldn't work out this machine's address: {e}")),
            },
            "--address" => {
                i += 1;
                match args.get(i) {
                    Some(a) => address = Some(a.clone()),
                    None => fail(
                        "--address needs a value, for example --address \\\\.\\pipe\\framekeep-test",
                    ),
                }
            }
            other => fail(&format!(
                "Unknown option `{other}`. Run with --help to see what this accepts."
            )),
        }
        i += 1;
    }

    // The messages are already written for a person to act on -- "Framekeep is
    // already running", not "os error 5".
    let daemon = match framekeep_tray::bring_up(address.as_deref()) {
        Ok(d) => d,
        Err(message) => fail(&message),
    };
    for line in &daemon.report {
        println!("{line}");
    }

    println!("Listening on {}", daemon.listener.address());
    println!("Press Ctrl+C to stop.");
    let _ = std::io::stdout().flush();

    let retention = daemon.retention.clone();
    if let Err(e) = framekeep_tray::serve(&daemon.listener, move || {
        framekeep_tray::connection_handlers(&retention)
    }) {
        fail(&format!("The connection point stopped working: {e}"));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
