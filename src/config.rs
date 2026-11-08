use crate::keys;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct K(pub u16);

impl Serialize for K {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&keys::name(self.0))
    }
}

impl<'de> Deserialize<'de> for K {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        keys::from_name(&s)
            .map(K)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown key name '{s}'")))
    }
}

fn d1() -> f64 {
    1.0
}
fn d5() -> f64 {
    5.0
}
fn d10() -> f64 {
    10.0
}
fn d20() -> f64 {
    20.0
}
fn d8() -> f64 {
    8.0
}
fn one() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub toggle_key: Option<K>,
    pub start_enabled: bool,
    pub exclusive: bool,
    pub record_stop_key: Option<K>,
    pub record_mouse_motion: bool,
    pub feedback_sound: bool,
    pub feedback_notify: bool,
    pub feedback_volume: f32,
    pub keyboard_layout: String,
    pub macros: Vec<Macro>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Hold,
    Toggle,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Macro {
    pub name: String,
    pub enabled: bool,
    pub trigger: Option<K>,
    pub modifiers: Vec<K>,
    pub mode: Mode,
    pub block_trigger: bool,
    pub action: Action,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Action {
    Repeat {
        #[serde(default)]
        keys: Vec<K>,
        #[serde(default = "d20")]
        interval_ms: f64,
        #[serde(default)]
        jitter_ms: f64,
        #[serde(default = "d5")]
        press_ms: f64,
        #[serde(default)]
        count: u32,
    },
    Sequence {
        #[serde(default)]
        steps: Vec<Step>,
        #[serde(default = "d1")]
        speed: f64,
        #[serde(default)]
        count: u32,
    },
    Type {
        #[serde(default)]
        text: String,
        #[serde(default = "d8")]
        char_ms: f64,
        #[serde(default = "d8")]
        hold_ms: f64,
        #[serde(default = "one")]
        count: u32,
    },
    Remap {
        #[serde(default)]
        to: Vec<K>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Step {
    Down {
        key: K,
    },
    Up {
        key: K,
    },
    Tap {
        key: K,
        #[serde(default = "d10")]
        hold_ms: f64,
    },
    Wait {
        ms: f64,
    },
    Move {
        dx: i32,
        dy: i32,
    },
    Wheel {
        amount: i32,
    },
    HWheel {
        amount: i32,
    },
    Type {
        text: String,
        #[serde(default = "d8")]
        char_ms: f64,
    },
}

impl Default for Action {
    fn default() -> Self {
        Action::Repeat {
            keys: vec![K(0x110)],
            interval_ms: 20.0,
            jitter_ms: 0.0,
            press_ms: 5.0,
            count: 0,
        }
    }
}

impl Default for Macro {
    fn default() -> Self {
        Macro {
            name: "New macro".into(),
            enabled: true,
            trigger: None,
            modifiers: vec![],
            mode: Mode::Hold,
            block_trigger: true,
            action: Action::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            toggle_key: Some(K(66)),
            start_enabled: true,
            exclusive: true,
            record_stop_key: Some(K(67)),
            record_mouse_motion: false,
            feedback_sound: true,
            feedback_notify: true,
            feedback_volume: 0.5,
            keyboard_layout: String::new(),
            macros: vec![
                Macro {
                    name: "Autoclicker (hold Mouse 5)".into(),
                    enabled: false,
                    trigger: Some(K(0x114)),
                    ..Default::default()
                },
                Macro {
                    name: "Spam E (toggle F7)".into(),
                    enabled: false,
                    trigger: Some(K(65)),
                    mode: Mode::Toggle,
                    action: Action::Repeat {
                        keys: vec![K(18)],
                        interval_ms: 50.0,
                        jitter_ms: 0.0,
                        press_ms: 10.0,
                        count: 0,
                    },
                    ..Default::default()
                },
                Macro {
                    name: "CapsLock -> Esc".into(),
                    enabled: false,
                    trigger: Some(K(58)),
                    action: Action::Remap { to: vec![K(1)] },
                    ..Default::default()
                },
                Macro {
                    name: "Combo example (Mouse 4)".into(),
                    enabled: false,
                    trigger: Some(K(0x113)),
                    mode: Mode::Toggle,
                    action: Action::Sequence {
                        steps: vec![
                            Step::Tap {
                                key: K(2),
                                hold_ms: 10.0,
                            },
                            Step::Wait { ms: 50.0 },
                            Step::Tap {
                                key: K(3),
                                hold_ms: 10.0,
                            },
                            Step::Wait { ms: 50.0 },
                            Step::Tap {
                                key: K(0x110),
                                hold_ms: 10.0,
                            },
                        ],
                        speed: 1.0,
                        count: 1,
                    },
                    ..Default::default()
                },
            ],
        }
    }
}

pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("evmacro")
        .join("config.toml")
}

pub fn try_load(path: &Path) -> Result<Config, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&s).map_err(|e| format!("config error in {}: {e}", path.display()))
}

pub fn load(path: &Path) -> (Config, Option<String>) {
    if !path.exists() {
        return (Config::default(), None);
    }
    match try_load(path) {
        Ok(c) => (c, None),
        Err(e) => {
            let bak = path.with_extension("toml.broken");
            let _ = std::fs::copy(path, &bak);
            (
                Config::default(),
                Some(format!(
                    "{e}\nUsing defaults; your file was backed up to {}",
                    bak.display()
                )),
            )
        }
    }
}

pub fn save(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}
