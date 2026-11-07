//! Chat data shared by every platform, the database and the screens.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Youtube,
    Twitch,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Youtube => "youtube",
            Platform::Twitch => "twitch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "youtube" => Some(Platform::Youtube),
            "twitch" => Some(Platform::Twitch),
            _ => None,
        }
    }
}

/// A piece of a message. Screens draw text with textContent and images only from allowed hosts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    Emoji { url: String, alt: String },
    Sticker { url: String, alt: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Badges {
    pub member: bool,
    pub moderator: bool,
    pub owner: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Colors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_bg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_bg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
}

pub mod kind {
    pub const TEXT: &str = "text";
    pub const PAID: &str = "paid";
    pub const STICKER: &str = "sticker";
    pub const MEMBERSHIP: &str = "membership";
    pub const GIFT_PURCHASE: &str = "gift_purchase";
    pub const GIFT_REDEEM: &str = "gift_redeem";
    pub const BITS: &str = "bits";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub platform: Platform,
    /// The platform's own message id.
    pub id: String,
    /// YouTube channel id or Twitch user id. Viewers are told apart by this, never by name.
    pub author_id: Option<String>,
    pub author: String,
    pub icon: Option<String>,
    pub kind: String,
    pub text: String,
    pub parts: Vec<Part>,
    /// Amount × 1,000,000, only for yen. Other currencies keep amount_text alone.
    pub amount_micros: Option<i64>,
    pub currency: Option<String>,
    pub amount_text: Option<String>,
    pub colors: Option<Colors>,
    pub badges: Badges,
    pub created_ms: i64,
}

impl ChatMessage {
    pub fn key(&self) -> String {
        message_key(self.platform, &self.id)
    }
}

pub fn message_key(platform: Platform, id: &str) -> String {
    format!("{}:{}", platform.as_str(), id)
}

/// A moderation action reported by the platform.
#[derive(Clone, Debug, PartialEq)]
pub enum Removal {
    Message(String),
    Author(String),
    /// Twitch's "clear chat" without a target.
    All,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub messages: Vec<ChatMessage>,
    pub removals: Vec<Removal>,
}

/// Image hosts the screens may load. Anything else is dropped before it is stored.
const IMAGE_HOST_SUFFIXES: [&str; 4] = [".ggpht.com", ".googleusercontent.com", ".ytimg.com", ".jtvnw.net"];

pub fn allowed_image(url: &str) -> Option<String> {
    let url = url.trim();
    let url = if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        url.to_string()
    };
    let parsed = url::Url::parse(&url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    IMAGE_HOST_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
        .then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_limited_to_platform_hosts() {
        assert_eq!(allowed_image("//yt3.ggpht.com/a.png").as_deref(), Some("https://yt3.ggpht.com/a.png"));
        assert_eq!(allowed_image("http://yt4.ggpht.com/a").as_deref(), Some("https://yt4.ggpht.com/a"));
        assert!(allowed_image("https://static-cdn.jtvnw.net/emoticons/v2/1/default/dark/1.0").is_some());
        assert_eq!(allowed_image("https://evil.example/a.png"), None);
        assert_eq!(allowed_image("https://ggpht.com.evil.example/a.png"), None);
        assert_eq!(allowed_image("javascript:alert(1)"), None);
        assert_eq!(allowed_image("data:image/png;base64,AAAA"), None);
    }
}
