//! Twitch chat through anonymous, read-only IRC (the same approach as YuiAIAssistant),
//! plus CLEARMSG and CLEARCHAT, which YuiAIAssistant did not follow.

use std::{collections::HashMap, time::Duration};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::model::{allowed_image, kind, Badges, Batch, ChatMessage, Part, Platform, Removal};

const HOST: &str = "irc.chat.twitch.tv";
const PORT: u16 = 6697;

/// Accepts `channel`, `@channel`, `tw:channel` or a twitch.tv URL.
pub fn parse_channel(input: &str) -> Result<String, String> {
    let mut value = input.trim().to_string();
    let lower = value.to_ascii_lowercase();
    for prefix in ["twitch:", "tw:"] {
        if lower.starts_with(prefix) {
            value = value[prefix.len()..].trim().to_string();
            break;
        }
    }
    let with_scheme = if value.to_ascii_lowercase().starts_with("twitch.tv/") || value.to_ascii_lowercase().starts_with("www.twitch.tv/") {
        format!("https://{value}")
    } else {
        value.clone()
    };
    if with_scheme.starts_with("http://") || with_scheme.starts_with("https://") {
        let url = url::Url::parse(&with_scheme).map_err(|_| "TwitchのURLを読み取れませんでした".to_string())?;
        let host = url.host_str().unwrap_or("");
        if host != "twitch.tv" && !host.ends_with(".twitch.tv") {
            return Err("TwitchのURLを入力してください".into());
        }
        value = url.path_segments().and_then(|mut s| s.find(|p| !p.is_empty())).unwrap_or("").to_string();
    }
    let channel = value.trim_start_matches('@').to_ascii_lowercase();
    let valid = (3..=25).contains(&channel.len()) && channel.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        Ok(channel)
    } else {
        Err("Twitchのチャンネル名を読み取れませんでした".into())
    }
}

fn unescape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[derive(Debug, Default, PartialEq)]
pub struct Line<'a> {
    pub tags: HashMap<&'a str, String>,
    pub prefix: &'a str,
    pub command: &'a str,
    pub params: Vec<&'a str>,
    pub trailing: Option<&'a str>,
}

pub fn parse_line(line: &str) -> Option<Line<'_>> {
    let mut rest = line.trim_end_matches(['\r', '\n']);
    let mut parsed = Line::default();
    if let Some(stripped) = rest.strip_prefix('@') {
        let (tags, after) = stripped.split_once(' ')?;
        for item in tags.split(';') {
            let (k, v) = item.split_once('=').unwrap_or((item, ""));
            parsed.tags.insert(k, unescape_tag(v));
        }
        rest = after;
    }
    if let Some(stripped) = rest.strip_prefix(':') {
        let (prefix, after) = stripped.split_once(' ')?;
        parsed.prefix = prefix;
        rest = after;
    }
    let (head, trailing) = match rest.split_once(" :") {
        Some((head, trailing)) => (head, Some(trailing)),
        None => (rest, None),
    };
    let mut words = head.split(' ').filter(|w| !w.is_empty());
    parsed.command = words.next()?;
    parsed.params = words.collect();
    parsed.trailing = trailing;
    Some(parsed)
}

/// Splits text by the `emotes` tag, whose positions count Unicode code points.
fn emote_parts(text: &str, emotes: &str) -> Vec<Part> {
    let chars: Vec<char> = text.chars().collect();
    let mut ranges: Vec<(usize, usize, String)> = Vec::new();
    for emote in emotes.split('/').filter(|e| !e.is_empty()) {
        let Some((id, positions)) = emote.split_once(':') else { continue };
        for position in positions.split(',') {
            if let Some((a, b)) = position.split_once('-') {
                if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    if a <= b && b < chars.len() {
                        ranges.push((a, b, id.to_string()));
                    }
                }
            }
        }
    }
    ranges.sort();
    let mut parts = Vec::new();
    let mut cursor = 0;
    for (a, b, id) in ranges {
        if a < cursor {
            continue;
        }
        if a > cursor {
            parts.push(Part::Text { text: chars[cursor..a].iter().collect() });
        }
        let alt: String = chars[a..=b].iter().collect();
        let url = format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/1.0");
        match allowed_image(&url) {
            Some(url) => parts.push(Part::Emoji { url, alt }),
            None => parts.push(Part::Text { text: alt }),
        }
        cursor = b + 1;
    }
    if cursor < chars.len() {
        parts.push(Part::Text { text: chars[cursor..].iter().collect() });
    }
    parts
}

fn badges_of(tag: Option<&String>) -> Badges {
    let mut badges = Badges::default();
    for badge in tag.map(String::as_str).unwrap_or("").split(',') {
        match badge.split('/').next().unwrap_or("") {
            "broadcaster" => badges.owner = true,
            "moderator" => badges.moderator = true,
            "subscriber" | "founder" => badges.member = true,
            _ => {}
        }
    }
    badges
}

pub enum Parsed {
    Message(ChatMessage),
    Removal(Removal),
    Ping(String),
    Reconnect,
}

pub fn parse_event(line: &str, now_ms: i64) -> Option<Parsed> {
    let parsed = parse_line(line)?;
    match parsed.command {
        "PING" => Some(Parsed::Ping(parsed.trailing.unwrap_or("tmi.twitch.tv").to_string())),
        "RECONNECT" => Some(Parsed::Reconnect),
        "CLEARMSG" => parsed.tags.get("target-msg-id").map(|id| Parsed::Removal(Removal::Message(id.clone()))),
        "CLEARCHAT" => Some(Parsed::Removal(match parsed.tags.get("target-user-id") {
            Some(user) if parsed.trailing.is_some() => Removal::Author(user.clone()),
            _ => Removal::All,
        })),
        "PRIVMSG" => {
            let mut text = parsed.trailing?.to_string();
            if let Some(action) = text.strip_prefix("\u{1}ACTION ") {
                text = action.trim_end_matches('\u{1}').to_string();
            }
            let tags = &parsed.tags;
            let nick = parsed.prefix.split('!').next().unwrap_or("");
            let author = tags.get("display-name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| nick.to_string());
            let created_ms = tags.get("tmi-sent-ts").and_then(|t| t.parse().ok()).unwrap_or(now_ms);
            let bits: Option<u64> = tags.get("bits").and_then(|b| b.parse().ok());
            let parts = emote_parts(&text, tags.get("emotes").map(String::as_str).unwrap_or(""));
            Some(Parsed::Message(ChatMessage {
                platform: Platform::Twitch,
                id: tags.get("id").cloned().unwrap_or_else(|| format!("{created_ms}_{author}_{text}")),
                author_id: tags.get("user-id").cloned(),
                author,
                icon: None,
                kind: if bits.is_some() { kind::BITS } else { kind::TEXT }.to_string(),
                text,
                parts,
                amount_micros: None,
                currency: None,
                amount_text: bits.map(|b| format!("{b} Bits")),
                colors: None,
                badges: badges_of(tags.get("badges")),
                created_ms,
            }))
        }
        _ => None,
    }
}

pub enum Event {
    Connected,
    Batch(Batch),
    Status(&'static str),
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

async fn session(channel: &str, cancel: &CancellationToken, emit: &mut impl FnMut(Event)) -> Result<(), String> {
    let tcp = tokio::net::TcpStream::connect((HOST, PORT)).await.map_err(|e| e.to_string())?;
    let connector = tokio_native_tls::TlsConnector::from(native_tls::TlsConnector::new().map_err(|e| e.to_string())?);
    let stream = connector.connect(HOST, tcp).await.map_err(|e| e.to_string())?;
    let (read, mut write) = tokio::io::split(stream);
    let nick = format!("justinfan{}", 10_000 + now_ms() % 80_000);
    let hello = format!("PASS SCHMOOPIIE\r\nNICK {nick}\r\nCAP REQ :twitch.tv/tags twitch.tv/commands\r\nJOIN #{channel}\r\n");
    write.write_all(hello.as_bytes()).await.map_err(|e| e.to_string())?;
    emit(Event::Connected);
    emit(Event::Status("running"));

    let mut lines = BufReader::new(read).lines();
    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            line = lines.next_line() => line.map_err(|e| e.to_string())?,
        };
        let Some(line) = line else { return Err("接続が切れました".into()) };
        match parse_event(&line, now_ms()) {
            Some(Parsed::Ping(token)) => {
                write.write_all(format!("PONG :{token}\r\n").as_bytes()).await.map_err(|e| e.to_string())?;
            }
            Some(Parsed::Reconnect) => return Err("Twitchから再接続を求められました".into()),
            Some(Parsed::Message(message)) => emit(Event::Batch(Batch { messages: vec![message], removals: vec![] })),
            Some(Parsed::Removal(removal)) => emit(Event::Batch(Batch { messages: vec![], removals: vec![removal] })),
            None => {}
        }
    }
}

/// Reads the channel until cancelled, reconnecting after drops.
pub async fn run(channel: String, cancel: CancellationToken, mut emit: impl FnMut(Event)) {
    let mut delay = Duration::from_secs(1);
    while !cancel.is_cancelled() {
        if session(&channel, &cancel, &mut emit).await.is_ok() {
            return;
        }
        emit(Event::Status("reconnecting"));
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_channel_inputs() {
        for input in ["shroud", "@Shroud", "tw:shroud", "twitch:shroud", "https://www.twitch.tv/shroud", "twitch.tv/shroud/videos"] {
            assert_eq!(parse_channel(input).as_deref(), Ok("shroud"), "{input}");
        }
        assert!(parse_channel("https://example.com/shroud").is_err());
        assert!(parse_channel("a").is_err());
        assert!(parse_channel("bad name").is_err());
    }

    #[test]
    fn parses_a_message_with_emotes_badges_and_markup() {
        let line = "@badges=moderator/1,subscriber/12;display-name=Viewer;emotes=25:0-4,16-20;id=m1;tmi-sent-ts=1700000000000;user-id=42 :viewer!viewer@viewer.tmi.twitch.tv PRIVMSG #chan :Kappa <b>hi</b> Kappa";
        let Some(Parsed::Message(m)) = parse_event(line, 0) else { panic!("message") };
        assert_eq!((m.id.as_str(), m.author_id.as_deref(), m.author.as_str()), ("m1", Some("42"), "Viewer"));
        assert_eq!(m.created_ms, 1_700_000_000_000);
        assert!(m.badges.moderator && m.badges.member && !m.badges.owner);
        assert_eq!(m.parts, vec![
            Part::Emoji { url: "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0".into(), alt: "Kappa".into() },
            Part::Text { text: " <b>hi</b> ".into() },
            Part::Emoji { url: "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0".into(), alt: "Kappa".into() },
        ]);
    }

    #[test]
    fn emote_positions_count_code_points() {
        let parts = emote_parts("😀 Kappa", "25:2-6");
        assert_eq!(parts[0], Part::Text { text: "😀 ".into() });
        assert!(matches!(&parts[1], Part::Emoji { alt, .. } if alt == "Kappa"));
    }

    #[test]
    fn bits_actions_and_escaped_names() {
        let line = r"@bits=100;display-name=A\sB;id=b1;user-id=7 :ab!ab@ab PRIVMSG #chan :Cheer100 thanks";
        let Some(Parsed::Message(m)) = parse_event(line, 5) else { panic!() };
        assert_eq!((m.kind.as_str(), m.amount_text.as_deref(), m.author.as_str(), m.created_ms), (kind::BITS, Some("100 Bits"), "A B", 5));
        let action = "@id=a1;user-id=7 :ab!ab@ab PRIVMSG #chan :\u{1}ACTION waves\u{1}";
        let Some(Parsed::Message(m)) = parse_event(action, 0) else { panic!() };
        assert_eq!(m.text, "waves");
    }

    #[test]
    fn follows_moderation_and_keeps_the_connection() {
        assert!(matches!(parse_event("@login=a;target-msg-id=m1 :tmi.twitch.tv CLEARMSG #chan :bad", 0), Some(Parsed::Removal(Removal::Message(id))) if id == "m1"));
        assert!(matches!(parse_event("@ban-duration=600;target-user-id=42 :tmi.twitch.tv CLEARCHAT #chan :viewer", 0), Some(Parsed::Removal(Removal::Author(id))) if id == "42"));
        assert!(matches!(parse_event("@room-id=1 :tmi.twitch.tv CLEARCHAT #chan", 0), Some(Parsed::Removal(Removal::All))));
        assert!(matches!(parse_event("PING :tmi.twitch.tv", 0), Some(Parsed::Ping(t)) if t == "tmi.twitch.tv"));
        assert!(matches!(parse_event(":tmi.twitch.tv RECONNECT", 0), Some(Parsed::Reconnect)));
        assert!(parse_event(":tmi.twitch.tv 001 justinfan1 :Welcome", 0).is_none());
    }
}
