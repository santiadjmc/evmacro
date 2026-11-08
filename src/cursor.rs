use parking_lot::{Condvar, Mutex};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Info {
    pub x: f64,
    pub y: f64,
    pub sx: f64,
    pub sy: f64,
    pub sw: f64,
    pub sh: f64,
}

struct Inbox {
    slot: Mutex<Option<Info>>,
    cv: Condvar,
}

struct Report {
    inbox: Arc<Inbox>,
}

#[zbus::interface(name = "io.github.evmacro.Cursor")]
impl Report {
    fn report(&self, s: String) {
        let v: Vec<f64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if let [x, y, sx, sy, sw, sh] = v[..] {
            *self.inbox.slot.lock() = Some(Info { x, y, sx, sy, sw, sh });
            self.inbox.cv.notify_all();
        }
    }
}

struct Kwin {
    conn: zbus::blocking::Connection,
    inbox: Arc<Inbox>,
    script: PathBuf,
    plugin: String,
    busy: Mutex<()>,
}

fn kwin() -> Result<&'static Kwin, String> {
    static K: OnceLock<Result<Kwin, String>> = OnceLock::new();
    K.get_or_init(|| {
        let pid = std::process::id();
        let bus = format!("io.github.evmacro.p{pid}");
        let inbox = Arc::new(Inbox {
            slot: Mutex::new(None),
            cv: Condvar::new(),
        });
        let conn = zbus::blocking::connection::Builder::session()
            .and_then(|b| b.name(bus.as_str()))
            .and_then(|b| b.serve_at("/Cursor", Report { inbox: inbox.clone() }))
            .and_then(|b| b.build())
            .map_err(|e| format!("D-Bus unavailable: {e}"))?;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let script = dir.join(format!("evmacro-cursor-{pid}.js"));
        let js = format!(
            "const p = workspace.cursorPos; const g = workspace.virtualScreenGeometry;\n\
             callDBus(\"{bus}\", \"/Cursor\", \"io.github.evmacro.Cursor\", \"Report\", \
             [p.x, p.y, g.x, g.y, g.width, g.height].join(\",\"));\n"
        );
        std::fs::write(&script, js).map_err(|e| format!("cannot write KWin script: {e}"))?;
        Ok(Kwin {
            conn,
            inbox,
            script,
            plugin: format!("evmacro_cursor_{pid}"),
            busy: Mutex::new(()),
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn scripting(
    k: &Kwin,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Result<zbus::Message> {
    k.conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        method,
        body,
    )
}

pub fn query() -> Result<Info, String> {
    let k = kwin()?;
    let _busy = k.busy.lock();
    *k.inbox.slot.lock() = None;
    let _ = scripting(k, "unloadScript", &k.plugin.as_str());
    let path = k.script.to_string_lossy().to_string();
    let id: i32 = scripting(k, "loadScript", &(path.as_str(), k.plugin.as_str()))
        .and_then(|m| m.body().deserialize())
        .map_err(|e| format!("KWin scripting unavailable (KDE Plasma required): {e}"))?;
    if id < 0 {
        return Err("KWin refused to load the cursor script".into());
    }
    let run = k.conn.call_method(
        Some("org.kde.KWin"),
        format!("/Scripting/Script{id}").as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    );
    let res = match run {
        Ok(_) => {
            let mut slot = k.inbox.slot.lock();
            if slot.is_none() {
                k.inbox.cv.wait_for(&mut slot, Duration::from_millis(600));
            }
            slot.take()
                .ok_or_else(|| "KWin did not report the cursor position".to_string())
        }
        Err(e) => Err(format!("KWin script failed: {e}")),
    };
    let _ = scripting(k, "unloadScript", &k.plugin.as_str());
    if let Ok(i) = res {
        *last_geometry().lock() = Some(i);
    }
    res
}

fn last_geometry() -> &'static Mutex<Option<Info>> {
    static G: Mutex<Option<Info>> = Mutex::new(None);
    &G
}

pub fn geometry() -> Result<Info, String> {
    if let Some(g) = *last_geometry().lock() {
        return Ok(g);
    }
    query()
}
