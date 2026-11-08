//! The overlay parts and how they look. Each part is its own OBS browser source, so where
//! it goes, how big it is and what it sits in front of or behind are all set in OBS.

use serde::{Deserialize, Serialize};

/// One part of the overlay, as an OBS browser source.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Part {
    pub id: &'static str,
    pub label: &'static str,
    /// The browser source's name in OBS.
    pub source: &'static str,
    pub width: u32,
    pub height: u32,
    /// Where the plugin first puts it on a 1920×1080 canvas.
    pub x: u32,
    pub y: u32,
}

/// Back to front: the plugin adds them in this order, so the last ends up in front.
pub const PARTS: [Part; 5] = [
    Part { id: "participants", label: "参加管理", source: "YuiLiveKit - 参加管理", width: 500, height: 600, x: 1380, y: 40 },
    Part { id: "ohayo", label: "おはよう", source: "YuiLiveKit - おはよう", width: 380, height: 120, x: 1500, y: 920 },
    Part { id: "comments", label: "通常コメント", source: "YuiLiveKit - コメント", width: 720, height: 636, x: 24, y: 420 },
    Part { id: "subtitles", label: "字幕", source: "YuiLiveKit - 字幕", width: 1600, height: 250, x: 160, y: 800 },
    Part { id: "niconico", label: "ニコニコ風", source: "YuiLiveKit - ニコニコ風", width: 1920, height: 880, x: 0, y: 100 },
];

pub const GROUP: &str = "YuiLiveKit";

pub fn part(id: &str) -> Option<&'static Part> {
    PARTS.iter().find(|p| p.id == id)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comments {
    pub font_size: i64,
    pub max: i64,
    pub background: String,
    pub text_color: String,
    pub author_color: String,
    pub show_icons: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Niconico {
    pub font_size: i64,
    pub duration: f64,
    pub color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Participants {
    pub font_size: i64,
    pub horizontal: bool,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ohayo {
    pub font_size: i64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subtitles {
    pub font_size: i64,
    pub english_size: i64,
    pub duration: f64,
    pub alignment: String,
    pub text_color: String,
    pub english_color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub comments: Comments,
    pub niconico: Niconico,
    pub participants: Participants,
    pub ohayo: Ohayo,
    pub subtitles: Subtitles,
}

impl Default for Comments {
    fn default() -> Self {
        Self {
            font_size: 24,
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
        Self { font_size: 56, duration: 8.0, color: "#ffffff".into() }
    }
}

impl Default for Participants {
    fn default() -> Self {
        Self { font_size: 24, horizontal: false, title: "参加待ち".into() }
    }
}

impl Default for Ohayo {
    fn default() -> Self {
        Self { font_size: 40, label: "おはよう".into() }
    }
}

impl Default for Subtitles {
    fn default() -> Self {
        Self {
            font_size: 46,
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

fn seconds(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if value.is_finite() { value.clamp(min, max) } else { fallback }
}

impl Appearance {
    /// Keeps every value in range, so saved settings can never break the overlay pages.
    pub fn sanitized(mut self) -> Self {
        let defaults = Appearance::default();
        for size in [
            &mut self.comments.font_size,
            &mut self.niconico.font_size,
            &mut self.participants.font_size,
            &mut self.ohayo.font_size,
            &mut self.subtitles.font_size,
            &mut self.subtitles.english_size,
        ] {
            *size = (*size).clamp(10, 200);
        }
        self.comments.max = self.comments.max.clamp(1, 50);
        self.comments.background = color(&self.comments.background, &defaults.comments.background);
        self.comments.text_color = color(&self.comments.text_color, &defaults.comments.text_color);
        self.comments.author_color = color(&self.comments.author_color, &defaults.comments.author_color);
        self.niconico.duration = seconds(self.niconico.duration, 2.0, 30.0, 8.0);
        self.niconico.color = color(&self.niconico.color, &defaults.niconico.color);
        self.participants.title = text(&self.participants.title, 40);
        self.ohayo.label = text(&self.ohayo.label, 40);
        self.subtitles.duration = seconds(self.subtitles.duration, 2.0, 60.0, 10.0);
        if !["left", "center", "right"].contains(&self.subtitles.alignment.as_str()) {
            self.subtitles.alignment = "center".into();
        }
        self.subtitles.text_color = color(&self.subtitles.text_color, &defaults.subtitles.text_color);
        self.subtitles.english_color = color(&self.subtitles.english_color, &defaults.subtitles.english_color);
        self
    }
}

pub fn from_setting(value: Option<serde_json::Value>) -> Appearance {
    value.and_then(|v| serde_json::from_value::<Appearance>(v).ok()).unwrap_or_default().sanitized()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_kept_in_range() {
        let mut appearance = Appearance::default();
        appearance.comments.font_size = 1000;
        appearance.comments.background = "red; background:url(x)".into();
        appearance.niconico.duration = f64::NAN;
        appearance.participants.title = "a\u{0}b".repeat(30);
        appearance.subtitles.alignment = "middle".into();
        let fixed = appearance.sanitized();
        assert_eq!(fixed.comments.font_size, 200);
        assert_eq!(fixed.comments.background, "#000000c7");
        assert_eq!(fixed.niconico.duration, 8.0);
        assert_eq!(fixed.participants.title.chars().count(), 40);
        assert!(!fixed.participants.title.contains('\u{0}'));
        assert_eq!(fixed.subtitles.alignment, "center");
    }

    #[test]
    fn missing_values_use_the_defaults() {
        let appearance = from_setting(Some(serde_json::json!({"comments": {"font_size": 30}})));
        assert_eq!(appearance.comments.font_size, 30);
        assert_eq!(appearance.comments.max, 12);
        assert_eq!(appearance.subtitles, Subtitles::default());
        assert_eq!(from_setting(None), Appearance::default());
    }

    #[test]
    fn parts_are_unique_and_fit_the_canvas() {
        for (i, part) in PARTS.iter().enumerate() {
            assert!(PARTS[i + 1..].iter().all(|other| other.id != part.id && other.source != part.source));
            assert!(part.x + part.width <= 1920 && part.y + part.height <= 1080, "{}", part.id);
        }
        assert_eq!(part("comments").unwrap().label, "通常コメント");
        assert!(part("layout").is_none());
    }
}
