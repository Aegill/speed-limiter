//! Настройки: лимиты приложений и параметры окна. Хранятся в JSON в профиле пользователя.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Unit {
    /// килобайты в секунду (1 КБ = 1024 Б)
    #[default]
    KBps,
    /// мегабайты в секунду
    MBps,
    /// мегабиты в секунду — в таких единицах обычно указана скорость тарифа
    Mbit,
}

impl Unit {
    pub const ALL: [Unit; 3] = [Unit::KBps, Unit::MBps, Unit::Mbit];

    pub fn label(self) -> &'static str {
        match self {
            Unit::KBps => "КБ/с",
            Unit::MBps => "МБ/с",
            Unit::Mbit => "Мбит/с",
        }
    }

    /// Сколько байт в секунду в одной единице.
    pub fn bytes_per_sec(self) -> f64 {
        match self {
            Unit::KBps => 1024.0,
            Unit::MBps => 1024.0 * 1024.0,
            Unit::Mbit => 125_000.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RuleCfg {
    pub enabled: bool,
    pub value: f64,
    pub unit: Unit,
}

pub fn limit_bps(value: f64, unit: Unit) -> Option<u64> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let bps = (value * unit.bytes_per_sec()).round();
    // от 1 КБ/с до ~10 ГБ/с
    Some((bps as u64).clamp(1024, 10_000_000_000))
}

/// Разбирает текст из поля ввода («1,5» и «1.5» одинаково).
pub fn parse_value(text: &str) -> Option<f64> {
    let v: f64 = text.trim().replace(',', ".").parse().ok()?;
    (v.is_finite() && v > 0.0).then_some(v)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub master: bool,
    pub dark: bool,
    pub show_all: bool,
    pub sort_by_speed: bool,
    pub rules: BTreeMap<String, RuleCfg>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            master: true,
            dark: true,
            show_all: false,
            sort_by_speed: true,
            rules: BTreeMap::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("SpeedLimiter").join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            // сначала во временный файл, чтобы не потерять настройки при сбое
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_convert() {
        assert_eq!(limit_bps(1.0, Unit::KBps), Some(1024));
        assert_eq!(limit_bps(1.0, Unit::MBps), Some(1_048_576));
        assert_eq!(limit_bps(8.0, Unit::Mbit), Some(1_000_000));
        assert_eq!(limit_bps(0.0, Unit::MBps), None);
        assert_eq!(limit_bps(-3.0, Unit::MBps), None);
        assert_eq!(limit_bps(f64::NAN, Unit::MBps), None);
    }

    #[test]
    fn parses_decimal_comma() {
        assert_eq!(parse_value("1,5"), Some(1.5));
        assert_eq!(parse_value(" 20 "), Some(20.0));
        assert_eq!(parse_value(""), None);
        assert_eq!(parse_value("abc"), None);
        assert_eq!(parse_value("0"), None);
    }

    #[test]
    fn config_roundtrip() {
        let mut c = Config::default();
        c.rules.insert(
            "chrome.exe".into(),
            RuleCfg { enabled: true, value: 2.5, unit: Unit::Mbit },
        );
        let json = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rules["chrome.exe"], c.rules["chrome.exe"]);
        // старый/неполный файл не должен ломать загрузку
        let partial: Config = serde_json::from_str("{\"dark\": false}").unwrap();
        assert!(!partial.dark && partial.master);
    }
}
