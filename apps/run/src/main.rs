//! `strake-run`: boot an Electron app directory headlessly (issue #110).
//!
//! The `npm start` equivalent for Strake's drop-in layer: reads the app
//! dir's `package.json`, runs its `main` script through the Electron shim,
//! marks the app ready, first-paints each created window's entry HTML, and
//! executes preload scripts. Headless by design (no display, no event loop):
//! handing a window to a real OS surface is the headed follow-up, which
//! consumes the reported window ids via `ShellWindow::attach`.
//!
//! Exit status is 0 only when the boot, the main script, and every preload
//! ran without errors; the full report prints either way. With
//! `--prove-ipc`, one IPC round-trip is additionally driven through the
//! booted app's own main/renderer pair (issue #84), and exit status is 0
//! only when that round-trip genuinely succeeds.

use std::path::PathBuf;

use strake_vibey_script::{BootOptions, IpcProof, boot_app_dir_with_options};

#[cfg(feature = "headed")]
mod headed;

fn usage() -> ! {
    eprintln!(
        "usage: strake-run [--prove-ipc] [--grant-fs <dir>...] [--prove-headed [--headed-secs N]] <app-dir>"
    );
    std::process::exit(2);
}

fn main() {
    // Index-based parsing (`--headed-secs` consumes the following argument):
    // a `for` loop cannot advance the iterator mid-body, and
    // `while let ... = args.next()` trips `clippy::while_let_on_iterator`.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut prove_ipc = false;
    let mut extra_fs_grants: Vec<PathBuf> = Vec::new();
    #[cfg(feature = "headed")]
    let mut prove_headed = false;
    #[cfg(feature = "headed")]
    let mut headed_secs = 6u64;
    let mut dir: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--prove-ipc" {
            prove_ipc = true;
        } else if arg == "--grant-fs" {
            i += 1;
            match args.get(i) {
                Some(granted) => extra_fs_grants.push(PathBuf::from(granted)),
                None => usage(),
            }
        } else if arg == "--prove-headed" {
            #[cfg(feature = "headed")]
            {
                prove_headed = true;
            }
            #[cfg(not(feature = "headed"))]
            {
                eprintln!("strake-run: --prove-headed needs the `headed` feature");
                std::process::exit(2);
            }
        } else if arg == "--headed-secs" {
            #[cfg(feature = "headed")]
            {
                i += 1;
                headed_secs = args.get(i).map(|n| n.parse().unwrap_or(6)).unwrap_or(6);
            }
            #[cfg(not(feature = "headed"))]
            {
                eprintln!("strake-run: --headed-secs needs the `headed` feature");
                std::process::exit(2);
            }
        } else if dir.is_none() {
            dir = Some(arg.clone());
        } else {
            usage();
        }
        i += 1;
    }
    #[cfg(feature = "headed")]
    if prove_headed {
        let Some(dir) = dir else { usage() };
        if prove_ipc {
            eprintln!("strake-run: --prove-ipc and --prove-headed are exclusive");
            std::process::exit(2);
        }
        match headed::prove_headed(&PathBuf::from(&dir), headed_secs) {
            Ok(()) => return,
            Err(error) => {
                eprintln!("strake-run: {error}");
                std::process::exit(1);
            }
        }
    }
    let Some(dir) = dir else { usage() };
    let mut ipc_proof = IpcProof::default();
    let options = BootOptions {
        extra_fs_grants,
        prove_ipc,
    };
    let report =
        match boot_app_dir_with_options(&PathBuf::from(&dir), &options).map(|(report, proof)| {
            ipc_proof = proof;
            report
        }) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("strake-run: {error}");
                std::process::exit(1);
            }
        };

    println!("app: {} (main: {})", report.app_name, report.main_entry);
    if report.js_errors.is_empty() {
        println!("main: ok, no JS errors");
    } else {
        println!("main: {} JS error(s):", report.js_errors.len());
        for error in &report.js_errors {
            println!("  - {error}");
        }
    }
    println!("windows: {}", report.windows.len());
    for window in &report.windows {
        println!(
            "  #{} {}x{} entry={}",
            window.id, window.width, window.height, window.entry_file
        );
        match &window.page_title {
            Some(title) => println!("    title: {title}"),
            None => println!("    title: (none)"),
        }
        match &window.preload {
            Some(preload) => {
                if window.preload_errors.is_empty() {
                    println!("    preload: {preload} (ok)");
                } else {
                    println!(
                        "    preload: {preload} ({} error(s))",
                        window.preload_errors.len()
                    );
                    for error in &window.preload_errors {
                        println!("      - {error}");
                    }
                }
            }
            None => println!("    preload: (none)"),
        }
    }

    if prove_ipc {
        match &ipc_proof.reply {
            Some(reply) => println!(
                "ipc: round-trip reply {reply:?} (pumped {})",
                ipc_proof.pumped
            ),
            None => println!("ipc: round-trip FAILED (pumped {})", ipc_proof.pumped),
        }
        for error in ipc_proof
            .main_errors
            .iter()
            .chain(ipc_proof.renderer_errors.iter())
        {
            println!("  - {error}");
        }
    }

    let failed = !report.js_errors.is_empty()
        || report
            .windows
            .iter()
            .any(|window| !window.preload_errors.is_empty())
        || (prove_ipc && !ipc_proof.succeeded());
    if failed {
        std::process::exit(1);
    }
}
