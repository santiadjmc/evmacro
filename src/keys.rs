use evdev::Key;
use std::collections::HashMap;
use std::sync::OnceLock;

struct Tables {
    list: Vec<(u16, String)>,
    by_name: HashMap<String, u16>,
    by_code: HashMap<u16, String>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut list = Vec::new();
        let mut by_name = HashMap::new();
        let mut by_code = HashMap::new();
        for code in 1..=0x2ffu16 {
            let n = format!("{:?}", Key::new(code));
            if (n.starts_with("KEY_") || n.starts_with("BTN_")) && !by_name.contains_key(&n) {
                by_name.insert(n.clone(), code);
                by_code.entry(code).or_insert_with(|| n.clone());
                list.push((code, n));
            }
        }
        Tables { list, by_name, by_code }
    })
}

pub fn all() -> &'static [(u16, String)] {
    &tables().list
}

pub fn name(code: u16) -> String {
    tables()
        .by_code
        .get(&code)
        .cloned()
        .unwrap_or_else(|| format!("CODE_{code}"))
}

pub fn from_name(s: &str) -> Option<u16> {
    let up = s.trim().to_ascii_uppercase();
    let t = tables();
    if let Some(&c) = t.by_name.get(&up) {
        return Some(c);
    }
    if let Some(&c) = t.by_name.get(&format!("KEY_{up}")) {
        return Some(c);
    }
    let num = up.strip_prefix("CODE_").unwrap_or(&up);
    num.parse::<u16>().ok().filter(|&c| c > 0 && c <= 0x2ff)
}

pub fn pretty(code: u16) -> String {
    match code {
        0x110 => "Mouse Left".into(),
        0x111 => "Mouse Right".into(),
        0x112 => "Mouse Middle".into(),
        0x113 => "Mouse 4 (Side)".into(),
        0x114 => "Mouse 5 (Extra)".into(),
        0x115 => "Mouse Forward".into(),
        0x116 => "Mouse Back".into(),
        0x117 => "Mouse Task".into(),
        _ => {
            let n = name(code);
            n.strip_prefix("KEY_").map(str::to_string).unwrap_or(n)
        }
    }
}
