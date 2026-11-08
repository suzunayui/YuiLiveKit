//! Where each part of the overlay goes on the 1920×1080 canvas, saved under a name so OBS
//! scenes can use different layouts (`/overlay?layout=<name>`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_NAME: &str = "default";
pub const CANVAS: (i64, i64) = (1920, 1080);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Area {
    pub visible: bool,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub font_size: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comments {
    #[serde(flatten)]
    pub area: Area,
    pub max: i64,
    pub background: String,
    pub text_color: String,
    pub author_color: String,
    pub show_icons: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Niconico {
    #[serde(flatten)]
    pub area: Area,
    pub duration: f64,
    pub color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Participants {
    #[serde(flatten)]
    pub area: Area,
    pub horizontal: bool,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ohayo {
    #[serde(flatten)]
    pub area: Area,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subtitles {
    #[serde(flatten)]
    pub area: Area,
    pub english_size: i64,
    pub duration: f64,
    pub alignment: String,
    pub text_color: String,
    pub english_color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub comments: Comments,
    pub niconico: Niconico,
    pub participants: Participants,
    pub ohayo: Ohayo,
    pub subtitles: Subtitles,
}

impl Default for Area {
    fn default() -> Self {
        Self { visible: true, x: 24, y: 420, width: 720, height: 636, font_size: 24 }
    }
}

impl Default for Comments {
    fn default() -> Self {
        Self {
            area: Area::default(),
            max: 12,
            background: "#000000c7".into(),
            text_color: "#ffffff".into(),
            author_color: "#ffd8f8".into(),
            show_icons: true,
        }
    }
}

impl Default for Niconico {
    fn default() -> Self {
        Self {
            area: Area { visible: false, x: 0, y: 100, width: 1920, height: 880, font_size: 56 },
            duration: 8.0,
            color: "#ffffff".into(),
        }
    }
}

impl Default for Participants {
    fn default() -> Self {
        Self {
            area: Area { visible: false, x: 1380, y: 40, width: 500, height: 600, font_size: 24 },
            horizontal: false,
            title: "参加待ち".into(),
        }
    }
}

impl Default for Ohayo {
    fn default() -> Self {
        Self { area: Area { visible: false, x: 1500, y: 920, width: 380, height: 120, font_size: 40 }, label: "おはよう".into() }
    }
}

impl Default for Subtitles {
    fn default() -> Self {
        Self {
            area: Area { visible: true, x: 160, y: 800, width: 1600, height: 250, font_size: 46 },
            english_size: 32,
            duration: 10.0,
            alignment: "center".into(),
            text_color: "#ffffff".into(),
            english_color: "#ffffff".into(),
        }
    }
}

fn color(value: &str, fallback: &str) -> String {
    let hex = value.strip_prefix('#').unwrap_or("");
    let valid = matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    if valid { format!("#{}", hex.to_ascii_lowercase()) } else { fallback.to_string() }
}

fn text(value: &str, max_chars: usize) -> String {
    value.chars().filter(|c| !c.is_control()).take(max_chars).collect()
}

impl Area {
    fn clamp(&mut self) {
        self.width = self.width.clamp(40, CANVAS.0);
        self.height = self.height.clamp(40, CANVAS.1);
        self.x = self.x.clamp(0, CANVAS.0 - self.width);
        self.y = self.y.clamp(0, CANVAS.1 - self.height);
        self.font_size = self.font_size.clamp(10, 200);
    }
}

impl Layout {
    /// Keeps every value in range, so a saved layout can never break the overlay page.
    pub fn sanitized(mut self) -> Self {
        let defaults = Layout::default();
        for area in [
            &mut self.comments.area,
            &mut self.niconico.area,
            &mut self.participants.area,
            &mut self.ohayo.area,
            &mut self.subtitles.area,
        ] {
            area.clamp();
        }
        self.comments.max = self.comments.max.clamp(1, 50);
        self.comments.background = color(&self.comments.background, &defaults.comments.background);
        self.comments.text_color = color(&self.comments.text_color, &defaults.comments.text_color);
        self.comments.author_color = color(&self.comments.author_color, &defaults.comments.author_color);
        self.niconico.duration = if self.niconico.duration.is_finite() { self.niconico.duration.clamp(2.0, 30.0) } else { 8.0 };
        self.niconico.color = color(&self.niconico.color, &defaults.niconico.color);
        self.participants.title = text(&self.participants.title, 40);
        self.ohayo.label = text(&self.ohayo.label, 40);
        self.subtitles.english_size = self.subtitles.english_size.clamp(10, 200);
        self.subtitles.duration = if self.subtitles.duration.is_finite() { self.subtitles.duration.clamp(2.0, 60.0) } else { 10.0 };
        if !["left", "center", "right"].contains(&self.subtitles.alignment.as_str()) {
            self.subtitles.alignment = "center".into();
        }
        self.subtitles.text_color = color(&self.subtitles.text_color, &defaults.subtitles.text_color);
        self.subtitles.english_color = color(&self.subtitles.english_color, &defaults.subtitles.english_color);
        self
    }
}

pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub type Layouts = BTreeMap<String, Layout>;

/// The saved layouts, always including the default one.
pub fn from_setting(value: Option<serde_json::Value>) -> Layouts {
    let mut layouts: Layouts = value
        .and_then(|v| serde_json::from_value::<Layouts>(v).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| valid_name(name))
        .map(|(name, layout)| (name, layout.sanitized()))
        .collect();
    layouts.entry(DEFAULT_NAME.into()).or_default();
    layouts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_kept_in_range() {
        let mut layout = Layout::default();
        layout.comments.area = Area { visible: true, x: 5000, y: -10, width: 99999, height: 1, font_size: 1000 };
        layout.comments.background = "red; background:url(x)".into();
        layout.niconico.duration = f64::NAN;
        layout.participants.title = "a\u{0}b".repeat(30);
        let fixed = layout.sanitized();
        assert_eq!(fixed.comments.area, Area { visible: true, x: 0, y: 0, width: 1920, height: 40, font_size: 200 });
        assert_eq!(fixed.comments.background, "#000000c7");
        assert_eq!(fixed.niconico.duration, 8.0);
        assert_eq!(fixed.participants.title.chars().count(), 40);
        assert!(!fixed.participants.title.contains('\u{0}'));
    }

    #[test]
    fn saved_layouts_always_include_default_and_drop_bad_names() {
        let value = serde_json::json!({"game": {"comments": {"x": 100}}, "bad name": {}});
        let layouts = from_setting(Some(value));
        assert_eq!(layouts.keys().collect::<Vec<_>>(), ["default", "game"]);
        assert_eq!(layouts["game"].comments.area.x, 100);
        assert_eq!(layouts["game"].comments.area.width, 720, "missing values use the defaults");
        assert!(valid_name("scene-1_A") && !valid_name("") && !valid_name("../x") && !valid_name("日本語"));
    }
}
