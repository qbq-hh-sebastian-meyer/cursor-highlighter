use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum ActivationMode {
    #[default]
    Toggle,
    Hold,
}

impl ActivationMode {
    pub fn visible(self, enabled: bool, held: bool) -> bool {
        enabled && (self == Self::Toggle || held)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Shortcut {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: u32,
}

impl Default for Shortcut {
    fn default() -> Self {
        Self {
            control: true,
            alt: true,
            shift: false,
            win: false,
            key: b'H' as u32,
        }
    }
}

impl Shortcut {
    pub fn validate(&self) -> Result<(), String> {
        if !(self.control || self.alt || self.win)
            || !((b'A' as u32..=b'Z' as u32).contains(&self.key)
                || (b'0' as u32..=b'9' as u32).contains(&self.key)
                || (0x70..=0x87).contains(&self.key))
        {
            return Err("Use Ctrl, Alt or Win (optionally Shift) with A-Z, 0-9 or F1-F24.".into());
        }
        Ok(())
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut shortcut = Self {
            control: false,
            alt: false,
            shift: false,
            win: false,
            key: 0,
        };
        for part in text.split('+').map(|part| part.trim().to_ascii_uppercase()) {
            let modifier = match part.as_str() {
                "CTRL" => Some(&mut shortcut.control),
                "ALT" => Some(&mut shortcut.alt),
                "SHIFT" => Some(&mut shortcut.shift),
                "WIN" => Some(&mut shortcut.win),
                _ => None,
            };
            if let Some(value) = modifier {
                if *value {
                    return Err("A shortcut modifier cannot be repeated.".into());
                }
                *value = true;
            } else {
                if shortcut.key != 0 {
                    return Err("Specify exactly one shortcut key.".into());
                }
                shortcut.key = match part.as_bytes() {
                    [key] if key.is_ascii_uppercase() || key.is_ascii_digit() => *key as u32,
                    _ => part
                        .strip_prefix('F')
                        .and_then(|number| number.parse::<u32>().ok())
                        .filter(|number| (1..=24).contains(number))
                        .map(|number| 0x6F + number)
                        .ok_or("Use a letter, digit or F1-F24 as the shortcut key.")?,
                };
            }
        }
        shortcut.validate()?;
        Ok(shortcut)
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        for (active, name) in [
            (self.control, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ] {
            if active {
                parts.push(name.to_string());
            }
        }
        parts.push(if (0x70..=0x87).contains(&self.key) {
            format!("F{}", self.key - 0x6F)
        } else {
            char::from_u32(self.key).unwrap_or('?').to_string()
        });
        parts.join(" + ")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Settings {
    pub diameter: u32,
    pub opacity: u32,
    pub color_hex: String,
    pub enabled: bool,
    pub shortcut: Shortcut,
    pub activation_mode: ActivationMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            diameter: 80,
            opacity: 35,
            color_hex: "#FFD43B".into(),
            enabled: true,
            shortcut: Shortcut::default(),
            activation_mode: ActivationMode::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(24..=240).contains(&self.diameter) || !(5..=80).contains(&self.opacity) {
            return Err("Settings require a diameter of 24-240 px and opacity of 5-80%.".into());
        }
        self.shortcut.validate()?;
        self.rgb().map(|_| ())
    }

    pub fn rgb(&self) -> Result<[u8; 3], String> {
        let hex = self.color_hex.as_bytes();
        if hex.len() != 7 || hex[0] != b'#' || !hex[1..].iter().all(u8::is_ascii_hexdigit) {
            return Err("The halo color must be a #RRGGBB value.".into());
        }
        let value = u32::from_str_radix(&self.color_hex[1..], 16).map_err(|e| e.to_string())?;
        Ok([(value >> 16) as u8, (value >> 8) as u8, value as u8])
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let settings: Self = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        use std::{io::Write, os::windows::ffi::OsStrExt};
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        self.validate()?;
        let parent = path.parent().ok_or("Settings path has no parent.")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temp = path.with_extension("json.tmp");
        let write = || -> Result<(), String> {
            let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            // Both paths are NUL-terminated and remain alive for the call.
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(())
        };
        let result = write();
        if result.is_err() && temp.exists() {
            fs::remove_file(&temp)
                .map_err(|e| format!("Failed to clean up settings temp file: {e}"))?;
        }
        result
    }
}

#[derive(Default)]
pub struct ClickFeedback {
    buttons: u8,
    pressed_at: u64,
    released_at: u64,
    active: bool,
}

impl ClickFeedback {
    pub fn button(&mut self, mask: u8, down: bool, now: u64) {
        let was_held = self.buttons != 0;
        if down {
            self.buttons |= mask;
        } else {
            self.buttons &= !mask;
        }
        if !was_held && self.buttons != 0 {
            self.pressed_at = now;
            self.active = true;
        } else if was_held && self.buttons == 0 {
            self.released_at = now.max(self.pressed_at + 120);
        }
    }

    pub fn amount(&self, now: u64) -> f32 {
        if !self.active {
            return 0.0;
        }
        if self.buttons != 0 || now <= self.released_at {
            return 1.0;
        }
        let progress = ((now - self.released_at) as f32 / 180.0).clamp(0.0, 1.0);
        1.0 - progress * progress * (3.0 - 2.0 * progress)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// A top-down premultiplied BGRA image, directly suitable for UpdateLayeredWindow.
pub fn render(settings: &Settings, feedback: f32) -> Result<Vec<u8>, String> {
    settings.validate()?;
    let rgb = settings.rgb()?;
    let amount = feedback.clamp(0.0, 1.0);
    let size = settings.diameter as usize + 4;
    let center = size as f32 / 2.0;
    let rx = settings.diameter as f32 * (1.0 - 0.04 * amount) / 2.0;
    let ry = settings.diameter as f32 * (1.0 - 0.18 * amount) / 2.0;
    let brightness = 1.0 - 0.35 * amount;
    let mut pixels = vec![0; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let mut covered = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = (x as f32 + (sx as f32 + 0.5) / 4.0 - center) / rx;
                    let dy = (y as f32 + (sy as f32 + 0.5) / 4.0 - center) / ry;
                    covered += u32::from(dx * dx + dy * dy <= 1.0);
                }
            }
            let alpha = (255.0 * settings.opacity as f32 / 100.0 * covered as f32 / 16.0).round();
            let pixel = &mut pixels[(y * size + x) * 4..][..4];
            for channel in 0..3 {
                pixel[channel] =
                    (rgb[2 - channel] as f32 * brightness * alpha / 255.0).round() as u8;
            }
            pixel[3] = alpha as u8;
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_defaults_and_csharp_compatibility() {
        let value: Settings = serde_json::from_str(
            r##"{"Diameter":137,"Opacity":58,"ColorHex":"#1256AB","Enabled":false}"##,
        )
        .unwrap();
        assert_eq!(value.diameter, 137);
        assert_eq!(value.rgb().unwrap(), [18, 86, 171]);
        assert!(!value.enabled);
        assert_eq!(value.shortcut, Shortcut::default());
        assert_eq!(value.activation_mode, ActivationMode::Toggle);
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
        assert!(serde_json::from_str::<Settings>("null").is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"Diameter":-1}"#).is_err());
    }

    #[test]
    fn shortcut_parsing_validation_and_labels() {
        assert_eq!(
            Shortcut::parse(" ctrl + alt + h ").unwrap(),
            Shortcut::default()
        );
        for key in ["A", "Z", "0", "9", "F1", "F24"] {
            let shortcut = Shortcut::parse(&format!("Win + Shift + {key}")).unwrap();
            shortcut.validate().unwrap();
            assert_eq!(Shortcut::parse(&shortcut.label()).unwrap(), shortcut);
        }
        for text in [
            "",
            "H",
            "Shift + H",
            "Ctrl",
            "Ctrl +",
            "Ctrl + + H",
            "Ctrl + Ctrl + H",
            "Ctrl + H + J",
            "Ctrl + F0",
            "Alt + F25",
            "Ctrl + Space",
            "Ctrl + 💛",
        ] {
            assert!(Shortcut::parse(text).is_err(), "{text}");
        }
        for key in [0, 0x20, 0x6F, 0x88, u32::MAX] {
            assert!(
                Shortcut {
                    key,
                    ..Shortcut::default()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn activation_modes_preserve_toggle_and_gate_hold_visibility() {
        for enabled in [false, true] {
            for held in [false, true] {
                assert_eq!(ActivationMode::Toggle.visible(enabled, held), enabled);
                assert_eq!(ActivationMode::Hold.visible(enabled, held), enabled && held);
            }
        }
    }

    #[test]
    fn activation_settings_round_trip_and_invalid_json() {
        let settings = Settings {
            shortcut: Shortcut::parse("Ctrl + Shift + F24").unwrap(),
            activation_mode: ActivationMode::Hold,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""ActivationMode":"Hold""#));
        assert!(json.contains(r#""Shortcut":{"Control":true"#));
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
        assert!(serde_json::from_str::<Settings>(r#"{"ActivationMode":"Unknown"}"#).is_err());
        assert!(
            serde_json::from_str::<Settings>(r#"{"Shortcut":{"Key":0}}"#)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_settings() {
        for diameter in [0, 23, 241, u32::MAX] {
            assert!(
                Settings {
                    diameter,
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
        for opacity in [0, 4, 81, u32::MAX] {
            assert!(
                Settings {
                    opacity,
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
        for color in ["red", "#GG0000", "#12345+", "# 12345", "#12345 ", "💛"] {
            assert!(
                Settings {
                    color_hex: color.into(),
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn atomic_settings_round_trip_and_corruption() {
        let dir =
            std::env::temp_dir().join(format!("CursorHighlighter.Rust-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let mut settings = Settings {
            diameter: 137,
            enabled: false,
            activation_mode: ActivationMode::Hold,
            shortcut: Shortcut::parse("Alt + Shift + F13").unwrap(),
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        settings.opacity = 65;
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(!path.with_extension("json.tmp").exists());
        fs::write(&path, "{broken").unwrap();
        assert!(Settings::load(&path).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn click_timing_hold_repeat_and_overlapping_buttons() {
        let mut click = ClickFeedback::default();
        assert_eq!(click.amount(0), 0.0);
        click.button(1, true, 100);
        assert_eq!(click.amount(10000), 1.0);
        click.button(1, false, 110);
        assert_eq!(click.amount(220), 1.0);
        assert_eq!(click.amount(310), 0.5);
        assert_eq!(click.amount(400), 0.0);
        click.button(1, true, 500);
        click.button(2, true, 510);
        click.button(1, false, 520);
        assert_eq!(click.amount(1000), 1.0);
        click.button(2, false, 1000);
        assert_eq!(click.amount(1090), 0.5);
        click.button(4, true, 1100);
        assert_eq!(click.amount(1100), 1.0);
        click.reset();
        assert_eq!(click.amount(1100), 0.0);
    }

    #[test]
    fn render_transparency_premultiplication_and_deformation() {
        for diameter in [24, 80, 240] {
            for opacity in [5, 35, 80] {
                let settings = Settings {
                    diameter,
                    opacity,
                    ..Settings::default()
                };
                let normal = render(&settings, 0.0).unwrap();
                let pressed = render(&settings, 1.0).unwrap();
                let size = diameter as usize + 4;
                let center = (size / 2 * size + size / 2) * 4;
                assert_eq!(normal.len(), size * size * 4);
                assert_eq!(&normal[0..4], &[0; 4]);
                assert_eq!(normal[center + 3], (opacity as f32 * 2.55).round() as u8);
                assert_eq!(pressed[center + 3], normal[center + 3]);
                for c in 0..3 {
                    assert!(
                        (pressed[center + c] as f32 - normal[center + c] as f32 * 0.65).abs()
                            <= 1.0
                    );
                }
                for pixel in normal
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .chain(pressed.as_chunks::<4>().0.iter())
                {
                    assert!(pixel[..3].iter().all(|&channel| channel <= pixel[3]));
                }
                let near_top = ((2 + (diameter as f32 * 0.04) as usize) * size + size / 2) * 4 + 3;
                assert!(normal[near_top] > 0);
                assert_eq!(pressed[near_top], 0);
            }
        }
    }
}
