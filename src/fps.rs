//! Fréquences d'images exactes (fractions), conversions secondes ↔ images et timecodes.
//!
//! Toutes les fréquences sont des fractions (30000/1001 et non 29.97) : sur un fichier
//! d'une heure, l'arrondi flottant de 29.97 décalerait l'image de plus de 3 images.

use regex::Regex;
use std::sync::OnceLock;

/// Marge pour absorber les erreurs flottantes quand un temps tombe pile sur une image.
const EPS: f64 = 1e-6;

/// Fraction strictement positive, toujours réduite.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Rate {
    pub num: i64,
    pub den: i64,
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a.abs()
    } else {
        gcd(b, a % b)
    }
}

impl Rate {
    pub const fn whole(num: i64) -> Rate {
        Rate { num, den: 1 }
    }

    pub fn new(num: i64, den: i64) -> Rate {
        let g = gcd(num, den).max(1);
        Rate { num: num / g, den: den / g }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Rapport de deux fréquences, en flottant.
    pub fn ratio(self, other: Rate) -> f64 {
        (self.num as f64 * other.den as f64) / (self.den as f64 * other.num as f64)
    }

    pub fn is_whole(self) -> bool {
        self.den == 1
    }

    /// « n/d », la forme stockée dans le cache.
    pub fn text(self) -> String {
        format!("{}/{}", self.num, self.den)
    }

    pub fn halved(self) -> Rate {
        Rate::new(self.num, self.den * 2)
    }
}

/// Fréquences reconnues, vers lesquelles on « aimante » les valeurs approximatives que
/// certains conteneurs renvoient (2997/100 → 30000/1001).
pub const STANDARD_RATES: [Rate; 13] = [
    Rate { num: 24000, den: 1001 },
    Rate::whole(24),
    Rate::whole(25),
    Rate { num: 30000, den: 1001 },
    Rate::whole(30),
    Rate { num: 48000, den: 1001 },
    Rate::whole(48),
    Rate::whole(50),
    Rate { num: 60000, den: 1001 },
    Rate::whole(60),
    Rate::whole(100),
    Rate { num: 120000, den: 1001 },
    Rate::whole(120),
];

/// Lit une fréquence ffprobe (« 30000/1001 », « 25/1 », « 0/0 ») ou décimale.
pub fn parse_rate(text: &str) -> Option<Rate> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some((a, b)) = text.split_once('/') {
        let num: i64 = a.trim().parse().ok()?;
        let den: i64 = b.trim().parse().ok()?;
        if num <= 0 || den <= 0 {
            return None;
        }
        return Some(Rate::new(num, den));
    }
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if frac.len() > 15 {
        return None;
    }
    let den = 10_i64.pow(frac.len() as u32);
    let digits = format!("{whole}{frac}");
    let num: i64 = digits.parse().ok()?;
    if num <= 0 {
        return None;
    }
    Some(Rate::new(num, den))
}

/// Remplace une fréquence quasi standard par sa valeur exacte (23.98 → 24000/1001).
///
/// La tolérance (0,03 %) est bien plus petite que l'écart entre 30 et 29,97 (0,1 %) :
/// ces deux fréquences ne sont jamais confondues.
pub fn normalize_rate(rate: Rate) -> Rate {
    for std in STANDARD_RATES {
        if (rate.ratio(std) - 1.0).abs() <= 3e-4 {
            return std;
        }
    }
    rate
}

/// Standard la plus proche d'une fréquence quelconque.
pub fn nearest_standard(value: f64) -> Rate {
    let mut best = STANDARD_RATES[0];
    for r in STANDARD_RATES {
        if (r.as_f64() - value).abs() < (best.as_f64() - value).abs() {
            best = r;
        }
    }
    best
}

/// Couple (timebase, ntsc) du XML Final Cut Pro 7 pour une fréquence donnée.
///
/// 29,97 → (30, true) ; 23,976 → (24, true) ; 59,94 → (60, true) ; 25 → (25, false).
/// Une fréquence non représentable (12,5…) est arrondie : voir [`is_xmeml_exact`].
pub fn xmeml_rate(rate: Rate) -> (i64, bool) {
    if rate.den == 1001 && rate.num % 1000 == 0 {
        return (rate.num / 1000, true);
    }
    if rate.den == 1 {
        return (rate.num, false);
    }
    ((rate.as_f64().round_ties_even() as i64).max(1), false)
}

pub fn rate_from_xmeml(timebase: i64, ntsc: bool) -> Rate {
    if ntsc {
        Rate::new(timebase * 1000, 1001)
    } else {
        Rate::whole(timebase)
    }
}

pub fn is_xmeml_exact(rate: Rate) -> bool {
    let (timebase, ntsc) = xmeml_rate(rate);
    rate_from_xmeml(timebase, ntsc) == rate
}

/// Nombre d'images par seconde « affiché » dans un timecode (30 pour 29,97).
pub fn nominal_fps(rate: Rate) -> i64 {
    (rate.as_f64().round_ties_even() as i64).max(1)
}

pub fn supports_drop_frame(rate: Rate) -> bool {
    rate.den == 1001 && matches!(nominal_fps(rate), 30 | 60)
}

pub fn seconds_to_frame_floor(seconds: f64, rate: Rate) -> i64 {
    (seconds * rate.num as f64 / rate.den as f64 + EPS).floor() as i64
}

pub fn seconds_to_frame_ceil(seconds: f64, rate: Rate) -> i64 {
    (seconds * rate.num as f64 / rate.den as f64 - EPS).ceil() as i64
}

pub fn frames_to_seconds(frames: i64, rate: Rate) -> f64 {
    (frames * rate.den) as f64 / rate.num as f64
}

fn timecode_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*(\d{1,2}):(\d{2}):(\d{2})([:;.,])(\d{2,3})\s*$").unwrap())
}

/// Convertit « HH:MM:SS:FF » (ou « ;FF » en drop-frame) en numéro d'image.
///
/// Renvoie (images depuis 00:00:00:00, drop_frame).
pub fn parse_timecode(timecode: &str, rate: Rate) -> Result<(i64, bool), String> {
    let caps = timecode_regex()
        .captures(timecode)
        .ok_or_else(|| format!("timecode illisible : {timecode:?}"))?;
    let part = |k: usize| caps[k].parse::<i64>().unwrap_or(0);
    let (hours, minutes, seconds, frames) = (part(1), part(2), part(3), part(5));
    let sep = &caps[4];
    let nominal = nominal_fps(rate);
    let drop = (sep == ";" || sep == ",") && supports_drop_frame(rate);
    let mut total = ((hours * 60 + minutes) * 60 + seconds) * nominal + frames;
    if drop {
        let dropped_per_minute = nominal / 15; // 2 à 29,97 ; 4 à 59,94
        let total_minutes = hours * 60 + minutes;
        total -= dropped_per_minute * (total_minutes - total_minutes / 10);
    }
    Ok((total, drop))
}

/// Inverse de [`parse_timecode`].
pub fn format_timecode(frames: i64, rate: Rate, drop: bool) -> String {
    let nominal = nominal_fps(rate);
    let mut frames = frames;
    let mut sep = ':';
    if drop && supports_drop_frame(rate) {
        let dropped = nominal / 15;
        let per_ten_minutes = nominal * 600 - dropped * 9;
        let per_minute = nominal * 60 - dropped;
        let tens = frames.div_euclid(per_ten_minutes);
        let rem = frames.rem_euclid(per_ten_minutes);
        frames += dropped * 9 * tens;
        if rem > dropped {
            frames += dropped * ((rem - dropped) / per_minute);
        }
        sep = ';';
    }
    let ff = frames.rem_euclid(nominal);
    let total_seconds = frames.div_euclid(nominal);
    let ss = total_seconds % 60;
    let mm = (total_seconds / 60) % 60;
    let hh = (total_seconds / 3600) % 24;
    format!("{hh:02}:{mm:02}:{ss:02}{sep}{ff:02}")
}

/// Affichage lisible : « 29,97 (30000/1001) ».
pub fn format_rate(rate: Option<Rate>) -> String {
    let Some(rate) = rate else {
        return "—".to_string();
    };
    let fixed = format!("{:.3}", rate.as_f64());
    let mut text = fixed.trim_end_matches('0').trim_end_matches('.').replace('.', ",");
    if rate.den != 1 {
        text += &format!(" ({}/{})", rate.num, rate.den);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC: Rate = Rate { num: 30000, den: 1001 };

    #[test]
    fn lit_les_frequences() {
        assert_eq!(parse_rate("30000/1001"), Some(NTSC));
        assert_eq!(parse_rate("25/1"), Some(Rate::whole(25)));
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(parse_rate("29.97"), Some(Rate::new(2997, 100)));
        assert_eq!(parse_rate(""), None);
    }

    #[test]
    fn aimante_sans_confondre_30_et_2997() {
        assert_eq!(normalize_rate(Rate::new(2997, 100)), NTSC);
        assert_eq!(normalize_rate(Rate::whole(30)), Rate::whole(30));
        assert_eq!(normalize_rate(Rate::new(2398, 100)), Rate::new(24000, 1001));
        assert_eq!(normalize_rate(Rate::new(25, 2)), Rate::new(25, 2));
    }

    #[test]
    fn frequences_xml() {
        assert_eq!(xmeml_rate(NTSC), (30, true));
        assert_eq!(xmeml_rate(Rate::whole(25)), (25, false));
        assert_eq!(xmeml_rate(Rate::new(25, 2)), (12, false));
        assert!(!is_xmeml_exact(Rate::new(25, 2)));
        assert!(is_xmeml_exact(NTSC));
    }

    #[test]
    fn timecode_aller_retour() {
        for rate in [NTSC, Rate::new(60000, 1001)] {
            for frame in [0, 1, 1799, 1800, 17982, 107892, 1_000_000] {
                let tc = format_timecode(frame, rate, true);
                assert!(tc.contains(';'));
                assert_eq!(parse_timecode(&tc, rate).unwrap(), (frame, true), "{tc}");
            }
        }
        for frame in [0, 24, 86399, 90000] {
            let tc = format_timecode(frame, Rate::whole(25), false);
            assert_eq!(parse_timecode(&tc, Rate::whole(25)).unwrap(), (frame, false));
        }
    }

    #[test]
    fn valeurs_connues_du_drop_frame() {
        assert_eq!(parse_timecode("00:01:00;02", NTSC).unwrap().0, 1800);
        assert_eq!(parse_timecode("00:10:00;00", NTSC).unwrap().0, 17982);
        assert_eq!(format_timecode(1800, NTSC, true), "00:01:00;02");
        assert!(parse_timecode("illisible", NTSC).is_err());
    }

    #[test]
    fn calage_sur_les_images() {
        assert_eq!(seconds_to_frame_floor(1.0, Rate::whole(25)), 25);
        assert_eq!(seconds_to_frame_ceil(1.0, Rate::whole(25)), 25);
        assert_eq!(seconds_to_frame_floor(1.0 - 1e-9, Rate::whole(25)), 25);
        assert_eq!(seconds_to_frame_ceil(1.001, Rate::whole(25)), 26);
    }

    #[test]
    fn affichage() {
        assert_eq!(format_rate(Some(NTSC)), "29,97 (30000/1001)");
        assert_eq!(format_rate(Some(Rate::whole(25))), "25");
        assert_eq!(format_rate(None), "—");
    }
}
