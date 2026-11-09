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

#[derive(Clone, Copy, Debug)]
pub enum PresentationPreset {
    Slides,
    LiveDemo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Settings {
    pub diameter: u32,
    pub opacity: u32,
    pub color_hex: String,
    pub enabled: bool,
    pub outline_only: bool,
    pub distinct_click_colors: bool,
    pub shortcut: Shortcut,
    pub activation_mode: ActivationMode,
    pub start_minimized: bool,
    pub scale_with_dpi: bool,
    pub auto_hide: bool,
    pub automatic_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            diameter: 80,
            opacity: 35,
            color_hex: "#FFD43B".into(),
            enabled: true,
            outline_only: false,
            distinct_click_colors: false,
            shortcut: Shortcut::default(),
            activation_mode: ActivationMode::default(),
            start_minimized: false,
            scale_with_dpi: false,
            auto_hide: false,
            automatic_updates: true,
        }
    }
}

impl Settings {
    pub fn pixel_diameter(&self, dpi: u32) -> Result<u32, String> {
        self.validate()?;
        if !self.scale_with_dpi {
            return Ok(self.diameter);
        }
        if dpi == 0 {
            return Err("The monitor reported zero DPI.".into());
        }
        self.diameter
            .checked_mul(dpi)
            .and_then(|value| value.checked_add(48))
            .map(|value| (value / 96).max(1))
            .ok_or_else(|| "The scaled halo diameter is too large.".into())
    }

    pub fn apply_preset(&mut self, preset: PresentationPreset) {
        let (diameter, opacity, color) = match preset {
            PresentationPreset::Slides => (60, 20, "#FFD43B"),
            PresentationPreset::LiveDemo => (120, 50, "#FF922B"),
        };
        self.diameter = diameter;
        self.opacity = opacity;
        self.color_hex = color.into();
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(24..=240).contains(&self.diameter) || !(5..=80).contains(&self.opacity) {
            return Err("Settings require a size of 24-240 and opacity of 5-80%.".into());
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

pub const IDLE_HIDE_MS: u64 = 1500;
const FADE_MS: u64 = 180;

pub struct AutoHide {
    last_moved: u64,
    typing: bool,
    updated_at: u64,
    visibility: f32,
}

impl AutoHide {
    pub fn new(now: u64) -> Self {
        Self {
            last_moved: now,
            typing: false,
            updated_at: now,
            visibility: 1.0,
        }
    }

    pub fn moved(&mut self, now: u64) {
        self.alpha(now);
        self.last_moved = now;
        self.typing = false;
    }

    pub fn typed(&mut self, now: u64) {
        self.alpha(now);
        self.typing = true;
    }

    pub fn alpha(&mut self, now: u64) -> u8 {
        let hidden = self.typing || now.saturating_sub(self.last_moved) >= IDLE_HIDE_MS;
        // Only time after the idle deadline contributes to the fade.
        let since = if hidden && !self.typing {
            self.updated_at
                .max(self.last_moved.saturating_add(IDLE_HIDE_MS))
        } else {
            self.updated_at
        };
        let step = now.saturating_sub(since) as f32 / FADE_MS as f32;
        self.visibility = (self.visibility + if hidden { -step } else { step }).clamp(0.0, 1.0);
        self.updated_at = now;
        (self.visibility * 255.0).round() as u8
    }
}

#[derive(Default)]
pub struct ClickFeedback {
    combined: PressFeedback,
    left: PressFeedback,
    right: PressFeedback,
}

impl ClickFeedback {
    pub fn button(&mut self, mask: u8, down: bool, now: u64) {
        self.combined.button(mask, down, now);
        if mask & 1 != 0 {
            self.left.button(1, down, now);
        }
        if mask & 2 != 0 {
            self.right.button(2, down, now);
        }
    }

    pub fn amount(&self, now: u64) -> f32 {
        self.combined.amount(now)
    }

    pub fn color_amounts(&self, now: u64) -> [f32; 2] {
        [self.left.amount(now), self.right.amount(now)]
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Default)]
struct PressFeedback {
    buttons: u8,
    pressed_at: u64,
    released_at: u64,
    active: bool,
}

impl PressFeedback {
    fn button(&mut self, mask: u8, down: bool, now: u64) {
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

    fn amount(&self, now: u64) -> f32 {
        if !self.active {
            return 0.0;
        }
        if self.buttons != 0 || now <= self.released_at {
            return 1.0;
        }
        let progress = ((now - self.released_at) as f32 / 180.0).clamp(0.0, 1.0);
        1.0 - progress * progress * (3.0 - 2.0 * progress)
    }
}

// A top-down premultiplied BGRA image, directly suitable for UpdateLayeredWindow.
pub fn render(
    settings: &Settings,
    feedback: f32,
    dpi: u32,
    color_amounts: [f32; 2],
) -> Result<Vec<u8>, String> {
    let diameter = settings.pixel_diameter(dpi)?;
    let rgb = settings.rgb()?;
    let amount = feedback.clamp(0.0, 1.0);
    let size = diameter as usize + 4;
    let center = size as f32 / 2.0;
    let rx = diameter as f32 * (1.0 - 0.04 * amount) / 2.0;
    let ry = diameter as f32 * (1.0 - 0.18 * amount) / 2.0;
    let outline_width = 5.0_f32.min(rx.min(ry) * 0.8);
    let brightness = 1.0 - 0.35 * amount;
    let mut pixels = vec![0; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let mut color = rgb.map(f32::from);
            if settings.distinct_click_colors {
                let [left, right] = color_amounts.map(|amount| amount.clamp(0.0, 1.0));
                // Split simultaneous clicks into blue/orange halves instead of mixing colors.
                let (tint, strength) = if left > 0.0 && (right == 0.0 || x < size / 2) {
                    ([51.0, 136.0, 255.0], left)
                } else {
                    ([255.0, 102.0, 51.0], right)
                };
                for channel in 0..3 {
                    color[channel] += (tint[channel] - color[channel]) * strength;
                }
            }
            let mut covered = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = (x as f32 + (sx as f32 + 0.5) / 4.0 - center) / rx;
                    let dy = (y as f32 + (sy as f32 + 0.5) / 4.0 - center) / ry;
                    let outer = dx * dx + dy * dy <= 1.0;
                    let inner = if settings.outline_only {
                        let inner_rx = rx - outline_width;
                        let inner_ry = ry - outline_width;
                        let inner_dx = (x as f32 + (sx as f32 + 0.5) / 4.0 - center) / inner_rx;
                        let inner_dy = (y as f32 + (sy as f32 + 0.5) / 4.0 - center) / inner_ry;
                        inner_dx * inner_dx + inner_dy * inner_dy <= 1.0
                    } else {
                        false
                    };
                    covered += u32::from(outer && !inner);
                }
            }
            let alpha = (255.0 * settings.opacity as f32 / 100.0 * covered as f32 / 16.0).round();
            let pixel = &mut pixels[(y * size + x) * 4..][..4];
            for channel in 0..3 {
                pixel[channel] = (color[2 - channel] * brightness * alpha / 255.0).round() as u8;
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
        assert!(!value.outline_only);
        assert!(!value.distinct_click_colors);
        assert_eq!(value.shortcut, Shortcut::default());
        assert_eq!(value.activation_mode, ActivationMode::Toggle);
        assert!(!value.start_minimized);
        assert!(!value.scale_with_dpi);
        assert!(!value.auto_hide);
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
    fn startup_visibility_is_opt_in_and_persisted() {
        let settings = Settings {
            start_minimized: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""StartMinimized":true"#));
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
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
    fn dpi_sizing_is_opt_in_and_rounds_to_nearest_pixel() {
        let mut settings = Settings::default();
        for dpi in [96, 120, 144, 168, 192, 240, 288, 384, 480] {
            assert_eq!(settings.pixel_diameter(dpi).unwrap(), 80);
        }
        settings.scale_with_dpi = true;
        for (dpi, expected) in [(96, 80), (120, 100), (144, 120), (192, 160), (480, 400)] {
            assert_eq!(settings.pixel_diameter(dpi).unwrap(), expected);
        }
        settings.diameter = 137;
        assert_eq!(settings.pixel_diameter(120).unwrap(), 171);
        assert_eq!(settings.pixel_diameter(144).unwrap(), 206);
        assert!(settings.pixel_diameter(0).is_err());
        assert!(settings.pixel_diameter(u32::MAX).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"ScaleWithDpi":"yes"}"#).is_err());
        assert!(
            serde_json::to_string(&settings)
                .unwrap()
                .contains(r#""ScaleWithDpi":true"#)
        );
    }

    #[test]
    fn dpi_rendering_preserves_logical_size_and_click_feedback_across_transitions() {
        for diameter in [24, 81, 137, 240] {
            let mut settings = Settings {
                diameter,
                scale_with_dpi: true,
                ..Settings::default()
            };
            for dpi in [96, 144, 192, 120, 96] {
                let pixels = settings.pixel_diameter(dpi).unwrap();
                assert!((pixels * 96).abs_diff(diameter * dpi) <= 48);
                for feedback in [0.0, 0.5, 1.0] {
                    let scaled = render(&settings, feedback, dpi, [0.0; 2]).unwrap();
                    let physical = render(
                        &Settings {
                            diameter: pixels,
                            ..Settings::default()
                        },
                        feedback,
                        96,
                        [0.0; 2],
                    );
                    if pixels <= 240 {
                        assert_eq!(scaled, physical.unwrap());
                    } else {
                        assert!(physical.is_err());
                    }
                    let size = pixels as usize + 4;
                    let center = (size / 2 * size + size / 2) * 4;
                    assert_eq!(scaled.len(), size * size * 4);
                    assert_eq!(&scaled[..4], &[0; 4]);
                    assert_eq!(
                        scaled[center + 3],
                        (settings.opacity as f32 * 2.55).round() as u8
                    );
                    for pixel in scaled.as_chunks::<4>().0 {
                        assert!(pixel[..3].iter().all(|&channel| channel <= pixel[3]));
                    }
                }
            }
            settings.scale_with_dpi = false;
            assert_eq!(
                render(&settings, 0.0, 192, [0.0; 2]).unwrap(),
                render(&settings, 0.0, 96, [0.0; 2]).unwrap()
            );
        }
    }

    #[test]
    fn presentation_presets_preserve_enabled_and_allow_customization() {
        for (enabled, auto_hide, scale_with_dpi) in [
            (false, false, false),
            (false, false, true),
            (false, true, false),
            (false, true, true),
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (true, true, true),
        ] {
            let mut settings = Settings {
                enabled,
                auto_hide,
                scale_with_dpi,
                start_minimized: true,
                outline_only: true,
                distinct_click_colors: true,
                activation_mode: ActivationMode::Hold,
                shortcut: Shortcut::parse("Ctrl + Shift + F9").unwrap(),
                ..Settings::default()
            };
            for preset in [PresentationPreset::Slides, PresentationPreset::LiveDemo] {
                settings.apply_preset(preset);
                settings.validate().unwrap();
                assert_eq!(settings.enabled, enabled);
                assert_eq!(settings.auto_hide, auto_hide);
                assert_eq!(settings.scale_with_dpi, scale_with_dpi);
                assert!(settings.start_minimized);
                assert!(settings.outline_only);
                assert!(settings.distinct_click_colors);
                assert_eq!(settings.activation_mode, ActivationMode::Hold);
                assert_eq!(
                    settings.shortcut,
                    Shortcut::parse("Ctrl + Shift + F9").unwrap()
                );
                let expected = match preset {
                    PresentationPreset::Slides => (60, 20, "#FFD43B"),
                    PresentationPreset::LiveDemo => (120, 50, "#FF922B"),
                };
                assert_eq!(
                    (
                        settings.diameter,
                        settings.opacity,
                        settings.color_hex.as_str()
                    ),
                    expected
                );
                assert_eq!(
                    settings.pixel_diameter(192).unwrap(),
                    expected.0 * if scale_with_dpi { 2 } else { 1 }
                );
                let text = serde_json::to_string(&settings).unwrap();
                assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), settings);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&text)
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .len(),
                    12
                );
                settings.diameter = 97;
                settings.opacity = 31;
                settings.color_hex = "#123456".into();
                settings.validate().unwrap();
                assert_eq!(settings.diameter, 97);
                assert_eq!(settings.opacity, 31);
                assert_eq!(settings.rgb().unwrap(), [18, 52, 86]);
            }
        }
    }

    #[test]
    fn presentation_presets_persist_with_existing_settings_schema() {
        let dir =
            std::env::temp_dir().join(format!("CursorHighlighter.Presets-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut settings = Settings {
            enabled: false,
            auto_hide: true,
            scale_with_dpi: true,
            activation_mode: ActivationMode::Hold,
            shortcut: Shortcut::parse("Alt + F13").unwrap(),
            ..Settings::default()
        };
        for preset in [PresentationPreset::Slides, PresentationPreset::LiveDemo] {
            settings.apply_preset(preset);
            settings.save(&path).unwrap();
            assert_eq!(Settings::load(&path).unwrap(), settings);
            settings.diameter = 97;
            settings.opacity = 31;
            settings.color_hex = "#123456".into();
            settings.save(&path).unwrap();
            assert_eq!(Settings::load(&path).unwrap(), settings);
        }
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
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
            distinct_click_colors: true,
            activation_mode: ActivationMode::Hold,
            shortcut: Shortcut::parse("Alt + Shift + F13").unwrap(),
            start_minimized: true,
            scale_with_dpi: true,
            auto_hide: true,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        settings.opacity = 65;
        settings.outline_only = true;
        settings.scale_with_dpi = false;
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(!path.with_extension("json.tmp").exists());
        fs::write(&path, "{broken").unwrap();
        assert!(Settings::load(&path).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn auto_hide_idle_typing_and_movement() {
        let mut hide = AutoHide::new(0);
        assert_eq!(hide.alpha(1499), 255);
        assert_eq!(hide.alpha(1500), 255);
        assert_eq!(hide.alpha(1590), 128);
        assert_eq!(hide.alpha(1680), 0);
        hide.moved(1680);
        assert_eq!(hide.alpha(1770), 128);
        assert_eq!(hide.alpha(1860), 255);
        hide.typed(1860);
        assert_eq!(hide.alpha(1950), 128);
        hide.typed(1950);
        assert_eq!(hide.alpha(2040), 0);
        assert_eq!(hide.alpha(10000), 0);
        hide.moved(10000);
        assert_eq!(hide.alpha(10180), 255);
        hide.moved(11000);
        assert_eq!(hide.alpha(12499), 255);
        assert_eq!(hide.alpha(12680), 0);
    }

    #[test]
    fn auto_hide_events_do_not_apply_past_time_to_new_target() {
        let mut hide = AutoHide::new(100);
        hide.typed(1000);
        assert_eq!(hide.alpha(1000), 255);
        assert_eq!(hide.alpha(1180), 0);
        hide.moved(10000);
        assert_eq!(hide.alpha(10000), 0);
        assert_eq!(hide.alpha(10180), 255);
        let mut reset = AutoHide::new(20000);
        assert_eq!(reset.alpha(21500), 255);
        assert_eq!(reset.alpha(21680), 0);
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
    fn distinct_button_timing_fades_independently_and_resets() {
        let mut click = ClickFeedback::default();
        assert_eq!(click.color_amounts(0), [0.0; 2]);
        click.button(1, true, 100);
        click.button(1, false, 110);
        click.button(2, true, 130);
        assert_eq!(click.color_amounts(220), [1.0, 1.0]);
        assert_eq!(click.color_amounts(310), [0.5, 1.0]);
        assert_eq!(click.color_amounts(400), [0.0, 1.0]);
        click.button(4, true, 400);
        click.button(2, false, 410);
        assert_eq!(click.color_amounts(500), [0.0, 0.5]);
        assert_eq!(click.amount(590), 1.0);
        assert_eq!(click.color_amounts(590), [0.0; 2]);
        click.button(1, true, 600);
        click.button(2, true, 610);
        assert_eq!(click.color_amounts(1000), [1.0; 2]);
        click.reset();
        assert_eq!(click.color_amounts(1000), [0.0; 2]);
        assert_eq!(click.amount(1000), 0.0);
    }

    #[test]
    fn distinct_colors_are_optional_fade_and_preserve_alpha() {
        for diameter in [24, 80, 240] {
            for opacity in [5, 35, 80] {
                let mut settings = Settings {
                    diameter,
                    opacity,
                    ..Settings::default()
                };
                let classic = render(&settings, 1.0, 96, [0.0; 2]).unwrap();
                assert_eq!(render(&settings, 1.0, 96, [1.0; 2]).unwrap(), classic);
                settings.distinct_click_colors = true;
                let left = render(&settings, 1.0, 96, [1.0, 0.0]).unwrap();
                let right = render(&settings, 1.0, 96, [0.0, 1.0]).unwrap();
                let both = render(&settings, 1.0, 96, [1.0; 2]).unwrap();
                let fading = render(&settings, 1.0, 96, [0.5, 0.0]).unwrap();
                assert_eq!(render(&settings, 1.0, 96, [0.0; 2]).unwrap(), classic);
                let size = diameter as usize + 4;
                let center = (size / 2 * size + size / 2) * 4;
                assert!(left[center] > left[center + 2]);
                assert!(right[center + 2] > right[center]);
                for (pixels, tint) in [
                    (&left, [255.0, 136.0, 51.0]),
                    (&right, [51.0, 102.0, 255.0]),
                ] {
                    for channel in 0..3 {
                        let expected = tint[channel] * 0.65 * classic[center + 3] as f32 / 255.0;
                        assert!((pixels[center + channel] as f32 - expected).abs() <= 1.0);
                    }
                }
                for channel in 0..3 {
                    let expected =
                        (classic[center + channel] as f32 + left[center + channel] as f32) / 2.0;
                    assert!((fading[center + channel] as f32 - expected).abs() <= 1.0);
                }
                for (index, pixel) in both.as_chunks::<4>().0.iter().enumerate() {
                    let offset = index * 4;
                    let expected = if index % size < size / 2 {
                        &left
                    } else {
                        &right
                    };
                    assert_eq!(pixel, &expected[offset..offset + 4]);
                }
                for pixels in [&left, &right, &both, &fading] {
                    for (pixel, original) in pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(classic.as_chunks::<4>().0)
                    {
                        assert_eq!(pixel[3], original[3]);
                        assert!(pixel[..3].iter().all(|&channel| channel <= pixel[3]));
                    }
                }
            }
        }
    }

    #[test]
    fn distinct_colors_follow_dpi_transitions() {
        let settings = Settings {
            diameter: 80,
            distinct_click_colors: true,
            scale_with_dpi: true,
            ..Settings::default()
        };
        for dpi in [96, 144, 192, 120, 96] {
            let diameter = settings.pixel_diameter(dpi).unwrap();
            for colors in [[1.0, 0.0], [0.0, 1.0], [1.0, 1.0], [0.5, 0.25]] {
                let scaled = render(&settings, 1.0, dpi, colors).unwrap();
                let physical = Settings {
                    diameter,
                    scale_with_dpi: false,
                    ..settings.clone()
                };
                assert_eq!(scaled, render(&physical, 1.0, 96, colors).unwrap());
            }
        }
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
                let normal = render(&settings, 0.0, 96, [0.0; 2]).unwrap();
                let pressed = render(&settings, 1.0, 96, [0.0; 2]).unwrap();
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

    #[test]
    fn outline_render_keeps_interior_clear_and_deforms_with_click_feedback() {
        for diameter in [24, 80, 240] {
            for opacity in [5, 35, 80] {
                let settings = Settings {
                    diameter,
                    opacity,
                    outline_only: true,
                    ..Settings::default()
                };
                let normal = render(&settings, 0.0, 96, [0.0; 2]).unwrap();
                let pressed = render(&settings, 1.0, 96, [0.0; 2]).unwrap();
                let rgb = settings.rgb().unwrap();
                let size = diameter as usize + 4;
                let center = size / 2;
                let center_pixel = (center * size + center) * 4;
                let ring_x = center + (diameter as f32 / 2.0 - 2.5).round() as usize;
                let ring_pixel = (center * size + ring_x) * 4;
                let pressed_ring_x = center + (diameter as f32 * 0.96 / 2.0 - 2.5).round() as usize;
                let pressed_ring_pixel = (center * size + pressed_ring_x) * 4;
                let inner_x = center + (diameter as f32 * 0.2).round() as usize;
                let inner_pixel = (center * size + inner_x) * 4;

                assert_eq!(normal[center_pixel + 3], 0);
                assert_eq!(pressed[center_pixel + 3], 0);
                assert!(normal[ring_pixel + 3] > 0);
                assert!(pressed[pressed_ring_pixel + 3] > 0);
                assert_eq!(normal[inner_pixel + 3], 0);
                assert_eq!(pressed[inner_pixel + 3], 0);
                for c in 0..3 {
                    let expected =
                        (rgb[2 - c] as f32 * 0.65 * pressed[pressed_ring_pixel + 3] as f32 / 255.0)
                            .round();
                    assert!((pressed[pressed_ring_pixel + c] as f32 - expected).abs() <= 1.0);
                }
                for pixel in normal
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .chain(pressed.as_chunks::<4>().0.iter())
                {
                    assert!(pixel[..3].iter().all(|&channel| channel <= pixel[3]));
                }
            }
        }
    }

    #[test]
    fn outline_render_tracks_dpi_scaling() {
        let settings = Settings {
            diameter: 80,
            outline_only: true,
            scale_with_dpi: true,
            ..Settings::default()
        };
        for dpi in [48, 96, 120, 144, 192] {
            let diameter = settings.pixel_diameter(dpi).unwrap();
            for feedback in [0.0, 0.5, 1.0] {
                let scaled = render(&settings, feedback, dpi, [0.0; 2]).unwrap();
                let physical = render(
                    &Settings {
                        diameter,
                        outline_only: true,
                        ..Settings::default()
                    },
                    feedback,
                    96,
                    [0.0; 2],
                )
                .unwrap();
                assert_eq!(scaled, physical);
                let size = diameter as usize + 4;
                let center = (size / 2 * size + size / 2) * 4;
                assert_eq!(&scaled[center..center + 4], &[0; 4]);
                assert!(scaled.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
            }
        }
    }

    #[test]
    fn distinct_click_colors_apply_to_outline_without_changing_alpha() {
        let settings = Settings {
            outline_only: true,
            distinct_click_colors: true,
            ..Settings::default()
        };
        let classic = render(&settings, 1.0, 96, [0.0; 2]).unwrap();
        let left = render(&settings, 1.0, 96, [1.0, 0.0]).unwrap();
        let right = render(&settings, 1.0, 96, [0.0, 1.0]).unwrap();
        let both = render(&settings, 1.0, 96, [1.0; 2]).unwrap();
        let size = settings.diameter as usize + 4;
        let center = size / 2;
        let left_pixel = (center * size + center - 36) * 4;
        let right_pixel = (center * size + center + 35) * 4;
        for (offset, pixels, tint) in [
            (left_pixel, &left, [255.0, 136.0, 51.0]),
            (right_pixel, &right, [51.0, 102.0, 255.0]),
        ] {
            assert!(pixels[offset + 3] > 0);
            for channel in 0..3 {
                let expected = tint[channel] * 0.65 * pixels[offset + 3] as f32 / 255.0;
                assert!((pixels[offset + channel] as f32 - expected).abs() <= 1.0);
            }
        }
        assert_eq!(
            &both[left_pixel..left_pixel + 4],
            &left[left_pixel..left_pixel + 4]
        );
        assert_eq!(
            &both[right_pixel..right_pixel + 4],
            &right[right_pixel..right_pixel + 4]
        );
        for image in [&left, &right, &both] {
            for (pixel, original) in image
                .as_chunks::<4>()
                .0
                .iter()
                .zip(classic.as_chunks::<4>().0)
            {
                assert_eq!(pixel[3], original[3]);
                assert!(pixel[..3].iter().all(|&channel| channel <= pixel[3]));
            }
        }
        let center_pixel = (center * size + center) * 4;
        assert_eq!(&both[center_pixel..center_pixel + 4], &[0; 4]);
    }
}
