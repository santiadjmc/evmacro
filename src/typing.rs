use crate::keys;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;
use xkbcommon::xkb;

pub const KEY_SHIFT: u16 = 42;
pub const KEY_ALTGR: u16 = 100;
const KEY_ENTER: u16 = 28;
const KEY_TAB: u16 = 15;
const KEY_SPACE: u16 = 57;

pub const MODIFIERS: [u16; 8] = [29, 97, 42, 54, 56, 100, 125, 126];

#[derive(Clone, Copy, Debug)]
pub struct Stroke {
    pub key: u16,
    pub shift: bool,
    pub altgr: bool,
}

pub struct Layout {
    pub name: String,
    chars: HashMap<char, Stroke>,
    dead: HashMap<char, Stroke>,
}

fn dead_mark(keysym_name: &str) -> Option<char> {
    Some(match keysym_name {
        "dead_grave" => '\u{300}',
        "dead_acute" => '\u{301}',
        "dead_circumflex" => '\u{302}',
        "dead_tilde" => '\u{303}',
        "dead_diaeresis" => '\u{308}',
        "dead_abovering" => '\u{30a}',
        "dead_caron" => '\u{30c}',
        "dead_cedilla" => '\u{327}',
        _ => return None,
    })
}

fn spacing_mark(c: char) -> Option<char> {
    Some(match c {
        '`' => '\u{300}',
        '´' => '\u{301}',
        '^' => '\u{302}',
        '~' => '\u{303}',
        '¨' => '\u{308}',
        '¸' => '\u{327}',
        _ => return None,
    })
}

fn is_keypad(code: u16) -> bool {
    matches!(code, 55 | 71..=83 | 96 | 98 | 117 | 118 | 121)
}

impl Layout {
    fn empty(name: &str) -> Layout {
        Layout {
            name: name.into(),
            chars: HashMap::new(),
            dead: HashMap::new(),
        }
    }

    fn build(spec: &str) -> Option<Layout> {
        let (layout, variant) = match spec.split_once('(') {
            Some((l, v)) => (l.trim(), v.trim_end_matches(')').trim()),
            None => (spec.trim(), ""),
        };
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let km = xkb::Keymap::new_from_names(&ctx, "", "", layout, variant, None, xkb::KEYMAP_COMPILE_NO_FLAGS)?;
        let shift = 1u32 << km.mod_get_index(xkb::MOD_NAME_SHIFT);
        let l3 = 1u32 << km.mod_get_index(xkb::MOD_NAME_ISO_LEVEL3_SHIFT);
        let mut st = xkb::State::new(&km);
        let ralt = xkb::keysym_get_name(st.key_get_one_sym(xkb::Keycode::new(KEY_ALTGR as u32 + 8)));
        let has_altgr = ralt == "ISO_Level3_Shift";

        let mut lay = Layout::empty(spec);
        for (mask, sh, ag) in [
            (0, false, false),
            (shift, true, false),
            (l3, false, true),
            (shift | l3, true, true),
        ] {
            if ag && !has_altgr {
                continue;
            }
            st.update_mask(mask, 0, 0, 0, 0, 0);
            for code in 1..=248u16 {
                if is_keypad(code) {
                    continue;
                }
                let kc = xkb::Keycode::new(code as u32 + 8);
                let stroke = Stroke {
                    key: code,
                    shift: sh,
                    altgr: ag,
                };
                let u = st.key_get_utf32(kc);
                if u >= 0x20 && u != 0x7f {
                    if let Some(ch) = char::from_u32(u) {
                        lay.chars.entry(ch).or_insert(stroke);
                    }
                } else if u == 0 {
                    if let Some(m) = dead_mark(&xkb::keysym_get_name(st.key_get_one_sym(kc))) {
                        lay.dead.entry(m).or_insert(stroke);
                    }
                }
            }
        }
        if lay.chars.is_empty() {
            return None;
        }
        Some(lay)
    }

    pub fn strokes(&self, c: char) -> Option<Vec<Stroke>> {
        let plain = |key| Stroke {
            key,
            shift: false,
            altgr: false,
        };
        if let Some(s) = self.chars.get(&c) {
            return Some(vec![*s]);
        }
        match c {
            '\n' | '\r' => return Some(vec![plain(KEY_ENTER)]),
            '\t' => return Some(vec![plain(KEY_TAB)]),
            _ => {}
        }
        if let Some(m) = spacing_mark(c) {
            if let Some(d) = self.dead.get(&m) {
                return Some(vec![*d, plain(KEY_SPACE)]);
            }
        }
        let d: Vec<char> = c.nfd().collect();
        if let [base, mark] = d[..] {
            if let (Some(dk), Some(b)) = (self.dead.get(&mark), self.chars.get(&base)) {
                return Some(vec![*dk, *b]);
            }
        }
        None
    }
}

pub fn detect() -> String {
    fn with_variant(l: &str, v: &str) -> String {
        if v.is_empty() {
            l.to_string()
        } else {
            format!("{l}({v})")
        }
    }
    if let Some(dir) = dirs::config_dir() {
        if let Ok(s) = std::fs::read_to_string(dir.join("kxkbrc")) {
            let get = |k: &str| {
                s.lines()
                    .find_map(|l| l.strip_prefix(k))
                    .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
                    .unwrap_or_default()
            };
            let l = get("LayoutList=");
            if !l.is_empty() {
                return with_variant(&l, &get("VariantList="));
            }
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf") {
        let get = |k: &str| {
            s.lines()
                .find(|l| l.contains(k))
                .and_then(|l| l.split('"').nth(3))
                .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
                .unwrap_or_default()
        };
        let l = get("\"XkbLayout\"");
        if !l.is_empty() {
            return with_variant(&l, &get("\"XkbVariant\""));
        }
    }
    if let Ok(l) = std::env::var("XKB_DEFAULT_LAYOUT") {
        if !l.is_empty() {
            return with_variant(&l, &std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default());
        }
    }
    "us".into()
}

pub fn layout(cfg_value: &str) -> Arc<Layout> {
    static CACHE: Mutex<Option<(String, Arc<Layout>)>> = Mutex::new(None);
    let spec = if cfg_value.trim().is_empty() {
        detect()
    } else {
        cfg_value.trim().to_string()
    };
    let mut c = CACHE.lock();
    if let Some((s, l)) = c.as_ref() {
        if *s == spec {
            return l.clone();
        }
    }
    let l = Arc::new(
        Layout::build(&spec)
            .or_else(|| Layout::build("us"))
            .unwrap_or_else(|| Layout::empty(&spec)),
    );
    *c = Some((spec, l.clone()));
    l
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Char(char),
    Combo { mods: Vec<u16>, key: u16, times: u32 },
    Down(u16),
    Up(u16),
    Wait(f64),
}

fn key_alias(name: &str) -> Option<u16> {
    let n = name.trim().to_ascii_lowercase();
    Some(match n.as_str() {
        "ctrl" | "control" | "lctrl" => 29,
        "rctrl" => 97,
        "shift" | "lshift" => 42,
        "rshift" => 54,
        "alt" | "lalt" => 56,
        "altgr" | "ralt" => 100,
        "win" | "super" | "meta" | "cmd" | "lwin" => 125,
        "rwin" => 126,
        "enter" | "return" => 28,
        "esc" | "escape" => 1,
        "tab" => 15,
        "space" => 57,
        "backspace" | "bs" => 14,
        "del" | "delete" => 111,
        "ins" | "insert" => 110,
        "home" => 102,
        "end" => 107,
        "pgup" | "pageup" => 104,
        "pgdn" | "pagedown" => 109,
        "up" => 103,
        "down" => 108,
        "left" => 105,
        "right" => 106,
        "caps" | "capslock" => 58,
        "click" | "lbutton" | "lclick" => 0x110,
        "rbutton" | "rclick" => 0x111,
        "mbutton" | "mclick" => 0x112,
        _ => return keys::from_name(&n),
    })
}

pub fn parse(text: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' if it.peek() == Some(&'{') => {
                it.next();
                out.push(Tok::Char('{'));
            }
            '}' if it.peek() == Some(&'}') => {
                it.next();
                out.push(Tok::Char('}'));
            }
            '{' => {
                let mut body = String::new();
                loop {
                    match it.next() {
                        Some('}') => break,
                        Some(ch) => body.push(ch),
                        None => return Err(format!("missing '}}' after '{{{body}'")),
                    }
                }
                out.push(parse_token(&body)?);
            }
            c => out.push(Tok::Char(c)),
        }
    }
    Ok(out)
}

fn parse_token(body: &str) -> Result<Tok, String> {
    if body.chars().count() == 1 {
        return Ok(Tok::Char(body.chars().next().unwrap()));
    }
    let b = body.trim();
    let mut words = b.split_whitespace();
    let head = words.next().ok_or("empty {}")?;
    let arg = words.next();
    if words.next().is_some() {
        return Err(format!("too many words in {{{b}}}"));
    }
    if matches!(head.to_ascii_lowercase().as_str(), "wait" | "sleep" | "delay") {
        let ms = arg
            .and_then(|a| a.parse::<f64>().ok())
            .filter(|ms| *ms >= 0.0)
            .ok_or_else(|| format!("{{{b}}}: expected milliseconds, e.g. {{Wait 100}}"))?;
        return Ok(Tok::Wait(ms));
    }
    let mut parts: Vec<&str> = head.split('+').collect();
    if head.ends_with("++") {
        parts.pop();
        parts.pop();
        parts.push("+");
    }
    let last = parts.pop().unwrap_or("");
    let mut mods = Vec::new();
    for m in parts {
        mods.push(key_alias(m).ok_or_else(|| format!("unknown key '{m}' in {{{b}}}"))?);
    }
    let key = match key_alias(last) {
        Some(k) => k,
        None if mods.is_empty() && arg.is_none() && last.chars().count() == 1 => {
            return Ok(Tok::Char(last.chars().next().unwrap()));
        }
        None => return Err(format!("unknown key '{last}' in {{{b}}}")),
    };
    match arg.map(|a| a.to_ascii_lowercase()) {
        None => Ok(Tok::Combo { mods, key, times: 1 }),
        Some(a) if a == "down" && mods.is_empty() => Ok(Tok::Down(key)),
        Some(a) if a == "up" && mods.is_empty() => Ok(Tok::Up(key)),
        Some(a) => match a.parse::<u32>() {
            Ok(n) if n >= 1 => Ok(Tok::Combo {
                mods,
                key,
                times: n.min(10_000),
            }),
            _ => Err(format!("{{{b}}}: expected a repeat count, 'down' or 'up'")),
        },
    }
}

pub fn untypable(text: &str, lay: &Layout) -> Vec<char> {
    let mut v: Vec<char> = Vec::new();
    if let Ok(toks) = parse(text) {
        for t in toks {
            if let Tok::Char(c) = t {
                if lay.strokes(c).is_none() && !v.contains(&c) {
                    v.push(c);
                }
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tokens() {
        let t = parse("Hi{Enter}{Ctrl+Shift+T}{Tab 3}{Wait 50}{{x}}{Shift down}{+}").unwrap();
        assert_eq!(t[0], Tok::Char('H'));
        assert_eq!(
            t[2],
            Tok::Combo {
                mods: vec![],
                key: 28,
                times: 1
            }
        );
        assert_eq!(
            t[3],
            Tok::Combo {
                mods: vec![29, 42],
                key: 20,
                times: 1
            }
        );
        assert_eq!(
            t[4],
            Tok::Combo {
                mods: vec![],
                key: 15,
                times: 3
            }
        );
        assert_eq!(t[5], Tok::Wait(50.0));
        assert_eq!(&t[6..9], &[Tok::Char('{'), Tok::Char('x'), Tok::Char('}')]);
        assert_eq!(t[9], Tok::Down(42));
        assert_eq!(t[10], Tok::Char('+'));
        assert!(parse("{Enter").is_err());
        assert!(parse("{Bogus}").is_err());
    }

    #[test]
    fn latam_layout() {
        let l = Layout::build("latam").expect("latam layout");
        let s = l.strokes('@').unwrap();
        assert!(s[0].altgr, "@ is AltGr+Q on latam");
        let s = l.strokes('A').unwrap();
        assert!(s[0].shift && s[0].key == 30);
        assert!(l.strokes('ñ').is_some());
        assert_eq!(l.strokes('á').unwrap().len(), 2, "á = dead acute + a");
    }

    #[test]
    fn us_layout() {
        let l = Layout::build("us").unwrap();
        let s = l.strokes('@').unwrap();
        assert!(s[0].shift && s[0].key == 3);
    }
}
