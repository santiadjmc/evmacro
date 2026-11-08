mod config;
mod engine;
mod feedback;
mod gui;
mod keys;
mod typing;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const HELP: &str = "\
evmacro - low-latency input macros for Linux (evdev/uinput, works on Wayland and X11)

USAGE:
  evmacro                 open the settings GUI (macros run while it is open)
  evmacro --headless      run without GUI; reloads the config file when it changes
  evmacro --config PATH   use a different config file
  evmacro --list-keys     print every key/button name usable in the config
  evmacro --test-feedback play the on/off feedback using your config's settings
";

fn main() {
    let mut headless = false;
    let mut path = config::default_path();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--headless" | "-H" => headless = true,
            "--config" | "-c" => match args.next() {
                Some(p) => path = PathBuf::from(p),
                None => die("--config needs a path"),
            },
            "--list-keys" => {
                for (c, n) in keys::all() {
                    println!("{c:>4}  {n:<24} {}", keys::pretty(*c));
                }
                return;
            }
            "--test-feedback" => {
                let (c, _) = config::load(&path);
                for on in [true, false] {
                    feedback::signal(on, c.feedback_sound, c.feedback_notify, c.feedback_volume);
                    std::thread::sleep(Duration::from_millis(900));
                }
                return;
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return;
            }
            other => die(&format!("unknown argument '{other}'\n\n{HELP}")),
        }
    }

    let (cfg, warn) = config::load(&path);
    if !path.exists() {
        if let Err(e) = config::save(&path, &cfg) {
            eprintln!("[evmacro] could not write default config: {e}");
        }
    }

    let sh = match engine::Shared::start(cfg.clone()) {
        Ok(s) => s,
        Err(e) => die(&format!(
            "cannot create the virtual input device (/dev/uinput): {e}\n\
             Run ./setup.sh once, then log out and back in."
        )),
    };
    if let Some(w) = warn {
        sh.log(w);
    }

    if headless {
        headless_loop(&sh, &path);
    } else if let Err(e) = gui::run(sh, cfg, path) {
        die(&format!("GUI failed to start: {e}\nYou can still use --headless."));
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn headless_loop(sh: &engine::Shared, path: &Path) -> ! {
    sh.log(format!("Headless mode, watching {}", path.display()));
    let mut last = mtime(path);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let now = mtime(path);
        if now != last {
            last = now;
            match config::try_load(path) {
                Ok(c) => {
                    sh.apply_config(c);
                    sh.log("Config reloaded");
                }
                Err(e) => sh.log(format!("{e}\n(keeping previous config)")),
            }
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("evmacro: {msg}");
    std::process::exit(1);
}
