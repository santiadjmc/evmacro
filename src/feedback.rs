use parking_lot::Mutex;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

const RATE: u32 = 48_000;

static PLAYER: Mutex<Option<Child>> = Mutex::new(None);
static NOTIFY_ID: AtomicU32 = AtomicU32::new(0);

pub fn signal(enabled: bool, sound: bool, notify: bool, volume: f32) {
    if !sound && !notify {
        return;
    }
    std::thread::spawn(move || {
        if sound {
            play(enabled, volume);
        }
        if notify {
            notification(enabled);
        }
    });
}

fn tone(enabled: bool, volume: f32) -> Vec<u8> {
    let notes: [(f32, f32); 2] = if enabled {
        [(660.0, 0.07), (990.0, 0.09)]
    } else {
        [(990.0, 0.07), (520.0, 0.09)]
    };
    let amp = volume.clamp(0.0, 1.0) * 0.6 * i16::MAX as f32;
    let fade = (RATE as f32 * 0.005) as usize;
    let mut pcm: Vec<i16> = Vec::new();
    for (freq, secs) in notes {
        let n = (RATE as f32 * secs) as usize;
        for i in 0..n {
            let env = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / RATE as f32).sin();
            pcm.push((s * env * amp) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

fn play(enabled: bool, volume: f32) {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(if enabled { "evmacro-on.wav" } else { "evmacro-off.wav" });
    if std::fs::write(&path, tone(enabled, volume)).is_err() {
        return;
    }
    let mut cur = PLAYER.lock();
    if let Some(mut c) = cur.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    for player in ["pw-play", "paplay", "aplay"] {
        let mut cmd = Command::new(player);
        if player == "aplay" {
            cmd.arg("-q");
        }
        if let Ok(c) = cmd
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            *cur = Some(c);
            return;
        }
    }
}

fn notification(enabled: bool) {
    let (body, icon) = if enabled {
        ("Macros ENABLED", "media-playback-start")
    } else {
        ("Macros DISABLED", "media-playback-pause")
    };
    let mut cmd = Command::new("notify-send");
    cmd.args(["-a", "evmacro", "-u", "low", "-t", "1500", "-e", "-i", icon, "-p"]);
    let prev = NOTIFY_ID.load(Relaxed);
    if prev != 0 {
        cmd.args(["-r", &prev.to_string()]);
    }
    cmd.args(["evmacro", body]);
    if let Ok(out) = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() {
        if let Ok(id) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
            NOTIFY_ID.store(id, Relaxed);
        }
    }
}
