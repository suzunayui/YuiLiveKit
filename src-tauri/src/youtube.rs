//! YouTube live chat through the unofficial InnerTube API (`get_live_chat`).
//! Ported from the Electron version's youtubeChat.js, including its later fixes.

use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::model::{allowed_image, kind, Badges, Batch, ChatMessage, Colors, Part, Platform, Removal};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/140 Safari/537.36";
/// Responses without a continuation in a row before the stream counts as ended.
pub const ENDED_POLLS_BEFORE_STOP: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Video(String),
    /// A channel path such as `/@handle` or `/channel/UC…`; its `/live` page leads to the stream.
    LivePath(String),
}

fn video_target(id: Option<&str>) -> Result<Target, String> {
    match id {
        Some(id) if is_video_id(id) => Ok(Target::Video(id.to_string())),
        _ => Err("URLから動画IDを読み取れませんでした".into()),
    }
}

fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn parse_url(text: &str) -> Result<Target, String> {
    let url = url::Url::parse(text).map_err(|_| "URLを読み取れませんでした".to_string())?;
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("music."))
        .unwrap_or(&host)
        .to_string();
    let segments: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    if host == "youtu.be" {
        return video_target(segments.first().copied());
    }
    if host != "youtube.com" {
        return Err("YouTubeのURLを入力してください".into());
    }
    let first = segments.first().copied().unwrap_or("");
    if first == "watch" || first == "live_chat" {
        let v = url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned());
        return video_target(v.as_deref());
    }
    if ["live", "shorts", "embed"].contains(&first) {
        return video_target(segments.get(1).copied());
    }
    if first.starts_with('@') {
        return Ok(Target::LivePath(format!("/{first}")));
    }
    if ["channel", "c", "user"].contains(&first) {
        if let Some(name) = segments.get(1) {
            return Ok(Target::LivePath(format!("/{first}/{name}")));
        }
    }
    Err("このURLからは配信を探せません。動画かチャンネルのURLを入力してください".into())
}

/// Accepts a video id, `@handle`, channel id or YouTube URL (with or without `https://`).
pub fn parse_target(input: &str) -> Result<Target, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("YouTubeの入力が空です".into());
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return parse_url(value);
    }
    let bare = ["youtube.com/", "www.youtube.com/", "m.youtube.com/", "music.youtube.com/", "youtu.be/"];
    if bare.iter().any(|prefix| lower.starts_with(prefix)) {
        return parse_url(&format!("https://{value}"));
    }
    if value.starts_with('@') {
        return Ok(Target::LivePath(format!("/{value}")));
    }
    if value.len() == 11 {
        return Ok(Target::Video(value.to_string()));
    }
    Ok(Target::LivePath(format!("/channel/{value}")))
}

#[derive(Debug, PartialEq)]
pub struct PageOptions {
    pub api_key: String,
    pub client_version: String,
    pub continuation: String,
}

fn capture(html: &str, pattern: &str) -> Option<String> {
    regex::Regex::new(pattern).ok()?.captures(html).map(|c| c[1].to_string())
}

pub fn extract_options(html: &str) -> Result<PageOptions, String> {
    let api_key = capture(html, r#""INNERTUBE_API_KEY"\s*:\s*"([^"]+)""#).ok_or("INNERTUBE_API_KEY が見つかりません")?;
    let client_version = capture(html, r#""clientVersion"\s*:\s*"([\d.]+)""#).ok_or("clientVersion が見つかりません")?;
    let continuation = capture(html, r#""continuation"\s*:\s*"([^"]+)""#).ok_or("チャットが見つかりません（配信中でない可能性があります）")?;
    Ok(PageOptions { api_key, client_version, continuation })
}

pub fn canonical_video_id(html: &str) -> Option<String> {
    capture(html, r#"<link rel="canonical" href="https://www\.youtube\.com/watch\?v=([^"&]+)">"#)
}

fn text_of(block: &Value) -> String {
    if let Some(simple) = block["simpleText"].as_str() {
        return simple.to_string();
    }
    runs_text(&block["runs"])
}

fn runs_text(runs: &Value) -> String {
    runs.as_array()
        .map(|runs| runs.iter().filter_map(|r| r["text"].as_str()).collect())
        .unwrap_or_default()
}

fn last_thumbnail(thumbnails: &Value) -> Option<String> {
    thumbnails.as_array()?.last()?["url"].as_str().and_then(allowed_image)
}

fn parse_parts(runs: &Value) -> Vec<Part> {
    let mut parts = Vec::new();
    for run in runs.as_array().into_iter().flatten() {
        if let Some(text) = run.get("text") {
            parts.push(Part::Text { text: text.as_str().unwrap_or("").to_string() });
        } else if let Some(emoji) = run.get("emoji") {
            let alt = emoji["shortcuts"][0]
                .as_str()
                .or_else(|| emoji["emojiId"].as_str())
                .unwrap_or("")
                .to_string();
            match last_thumbnail(&emoji["image"]["thumbnails"]) {
                Some(url) => parts.push(Part::Emoji { url, alt }),
                None => parts.push(Part::Text { text: alt }),
            }
        }
    }
    parts
}

fn plain_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn to_hex(value: &Value) -> Option<String> {
    value.as_i64().or_else(|| value.as_u64().map(|v| v as i64)).map(|v| format!("#{:06X}", v & 0xff_ffff))
}

/// The tiers on the overlay are in yen, so only yen gets an amount (K8).
pub fn parse_yen(text: &str) -> Option<i64> {
    let rest = text.trim_start();
    let rest = rest.strip_prefix("JP").unwrap_or(rest);
    let rest = rest.strip_prefix('¥').or_else(|| rest.strip_prefix('￥'))?;
    let digits: String = rest.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit() || *c == ',').filter(|c| *c != ',').collect();
    digits.parse::<i64>().ok().map(|yen| yen * 1_000_000)
}

fn parse_badges(renderer: &Value) -> Badges {
    let mut badges = Badges::default();
    for badge in renderer["authorBadges"].as_array().into_iter().flatten() {
        let r = &badge["liveChatAuthorBadgeRenderer"];
        let icon = r["icon"]["iconType"].as_str().unwrap_or("").to_ascii_uppercase();
        let raw = format!(
            "{} {}",
            r["tooltip"].as_str().unwrap_or(""),
            r["accessibility"]["accessibilityData"]["label"].as_str().unwrap_or("")
        );
        let lower = raw.to_lowercase();
        if icon.contains("OWNER") || lower.contains("owner") || raw.contains("所有者") || raw.contains("配信者") {
            badges.owner = true;
        }
        if icon.contains("MODERATOR") || lower.contains("moderator") || raw.contains("モデレーター") || raw.contains("管理者") {
            badges.moderator = true;
        }
        if icon.contains("MEMBER") || lower.contains("member") || raw.contains("メンバー") || raw.contains("スポンサー") {
            badges.member = true;
        }
    }
    badges
}

fn parse_item(item: &Value, index: usize) -> Option<ChatMessage> {
    let (renderer, msg_kind) = [
        ("liveChatTextMessageRenderer", kind::TEXT),
        ("liveChatPaidMessageRenderer", kind::PAID),
        ("liveChatPaidStickerRenderer", kind::STICKER),
        ("liveChatMembershipItemRenderer", kind::MEMBERSHIP),
        ("liveChatSponsorshipsGiftPurchaseAnnouncementRenderer", kind::GIFT_PURCHASE),
        ("liveChatGiftRedemptionAnnouncementRenderer", kind::GIFT_REDEEM),
    ]
    .iter()
    .find_map(|(name, k)| item.get(*name).map(|r| (r, *k)))?;

    let mut author = text_of(&renderer["authorName"]);
    let usec: i64 = renderer["timestampUsec"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
    let created_ms = usec / 1000;
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut amount_text = None;
    let mut colors = None;
    let mut icon = last_thumbnail(&renderer["authorPhoto"]["thumbnails"]);

    match msg_kind {
        kind::TEXT => {
            parts = parse_parts(&renderer["message"]["runs"]);
            text = plain_text(&parts);
        }
        kind::PAID => {
            parts = parse_parts(&renderer["message"]["runs"]);
            text = plain_text(&parts);
            amount_text = renderer["purchaseAmountText"]["simpleText"].as_str().map(str::to_string);
            colors = Some(Colors {
                header_bg: to_hex(&renderer["headerBackgroundColor"]),
                header_text: to_hex(&renderer["headerTextColor"]),
                body_bg: to_hex(&renderer["bodyBackgroundColor"]),
                body_text: to_hex(&renderer["bodyTextColor"]),
            });
        }
        kind::STICKER => {
            let sticker = &renderer["sticker"];
            let alt = sticker["accessibility"]["accessibilityData"]["label"].as_str().unwrap_or("").to_string();
            if let Some(url) = last_thumbnail(&sticker["thumbnails"]) {
                parts.push(Part::Sticker { url, alt });
            }
            text = "[STICKER]".into();
            amount_text = renderer["purchaseAmountText"]["simpleText"].as_str().map(str::to_string);
            let text_color = if renderer.get("moneyChipTextColor").is_some() {
                &renderer["moneyChipTextColor"]
            } else {
                &renderer["authorNameTextColor"]
            };
            colors = Some(Colors {
                body_bg: to_hex(&renderer["backgroundColor"]),
                body_text: to_hex(text_color),
                ..Colors::default()
            });
        }
        kind::MEMBERSHIP => {
            parts = parse_parts(&renderer["message"]["runs"]);
            let pieces = [
                runs_text(&renderer["headerPrimaryText"]["runs"]),
                runs_text(&renderer["headerSubtext"]["runs"]),
                plain_text(&parts),
            ];
            text = pieces.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                text = "[MEMBERSHIP]".into();
            }
        }
        kind::GIFT_PURCHASE => {
            let header = &renderer["header"]["liveChatSponsorshipsHeaderRenderer"];
            let mut name = text_of(&header["authorName"]).trim_start_matches('@').to_string();
            if name.is_empty() {
                name = runs_text(&header["primaryText"]["runs"]).split_whitespace().next().unwrap_or("").to_string();
            }
            if icon.is_none() {
                icon = last_thumbnail(&header["authorPhoto"]["thumbnails"]);
            }
            text = if name.is_empty() {
                "誰かがギフトをくれました！".into()
            } else {
                author = name.clone();
                format!("{name} さんがギフトをくれました！")
            };
            parts = vec![Part::Text { text: text.clone() }];
        }
        kind::GIFT_REDEEM => {
            let header = runs_text(&renderer["header"]["runs"]);
            let sub = runs_text(&renderer["subtext"]["runs"]);
            text = [header.clone(), sub].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                text = "[GIFT REDEEM]".into();
            }
            parts = parse_parts(&renderer["message"]["runs"]);
            if parts.is_empty() {
                parts = vec![Part::Text { text: text.clone() }];
            }
            if author.is_empty() {
                if let Some((guess, _)) = header.split_once("さん") {
                    author = guess.to_string();
                }
            }
        }
        _ => {}
    }

    if author.is_empty() {
        author = match msg_kind {
            kind::GIFT_PURCHASE | kind::GIFT_REDEEM => "ギフト",
            kind::MEMBERSHIP => "メンバー",
            _ => "？？？",
        }
        .into();
    }

    let mut badges = parse_badges(renderer);
    badges.member |= msg_kind == kind::MEMBERSHIP;
    let id = ["id", "messageId", "trackingParams"]
        .iter()
        .find_map(|k| renderer[*k].as_str().filter(|s| !s.is_empty()))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{created_ms}_{author}_{text}_{index}"));
    let amount_micros = amount_text.as_deref().and_then(parse_yen);

    Some(ChatMessage {
        platform: Platform::Youtube,
        id,
        author_id: renderer["authorExternalChannelId"].as_str().map(str::to_string),
        author,
        icon,
        kind: msg_kind.to_string(),
        text,
        parts,
        currency: amount_micros.map(|_| "JPY".to_string()),
        amount_micros,
        amount_text,
        colors,
        badges,
        created_ms,
    })
}

fn parse_removal(action: &Value) -> Option<Removal> {
    for key in ["removeChatItemAction", "markChatItemAsDeletedAction"] {
        if let Some(id) = action[key]["targetItemId"].as_str() {
            return Some(Removal::Message(id.to_string()));
        }
    }
    for key in ["removeChatItemByAuthorAction", "markChatItemsByAuthorAsDeletedAction"] {
        if let Some(channel) = action[key]["externalChannelId"].as_str() {
            return Some(Removal::Author(channel.to_string()));
        }
    }
    None
}

#[derive(Debug, PartialEq)]
pub struct Poll {
    pub batch: Batch,
    /// `None` when the response has no continuation, which is how an ended stream looks.
    pub next: Option<(String, u64)>,
}

pub fn parse_poll(data: &Value) -> Poll {
    let live = &data["continuationContents"]["liveChatContinuation"];
    let mut batch = Batch::default();
    for (index, action) in live["actions"].as_array().into_iter().flatten().enumerate() {
        if let Some(removal) = parse_removal(action) {
            batch.removals.push(removal);
        } else if let Some(message) = parse_item(&action["addChatItemAction"]["item"], index) {
            batch.messages.push(message);
        }
    }
    let first = &live["continuations"][0];
    let next = ["timedContinuationData", "invalidationContinuationData"].iter().find_map(|key| {
        let block = &first[*key];
        block["continuation"]
            .as_str()
            .map(|c| (c.to_string(), block["timeoutMs"].as_u64().unwrap_or(1000)))
    });
    Poll { batch, next }
}

/// The first response is "Top chat"; this finds the continuation for "All chat".
pub fn all_chat_continuation(data: &Value) -> Option<String> {
    let items = &data["continuationContents"]["liveChatContinuation"]["header"]["liveChatHeaderRenderer"]["viewSelector"]
        ["sortFilterSubMenuRenderer"]["subMenuItems"];
    items.as_array()?.iter().find_map(|item| {
        if item["selected"].as_bool() == Some(true) {
            return None;
        }
        item["continuation"]["reloadContinuationData"]["continuation"].as_str().map(str::to_string)
    })
}

/// Counts responses without a continuation; one alone is treated as a hiccup (K6).
#[derive(Default)]
pub struct EndTracker {
    empty: u32,
}

impl EndTracker {
    /// Returns true once the stream should be treated as ended.
    pub fn record(&mut self, had_continuation: bool) -> bool {
        self.empty = if had_continuation { 0 } else { self.empty + 1 };
        self.empty >= ENDED_POLLS_BEFORE_STOP
    }
}

pub enum Event {
    Stream { video_id: String },
    Batch(Batch),
    Status(&'static str),
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .expect("HTTP client")
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("YouTubeの取得に失敗しました ({})", response.status()));
    }
    response.text().await.map_err(|e| e.to_string())
}

pub async fn resolve_video_id(client: &reqwest::Client, target: &Target) -> Result<Option<String>, String> {
    match target {
        Target::Video(id) => Ok(Some(id.clone())),
        Target::LivePath(path) => {
            let html = get_text(client, &format!("https://www.youtube.com{path}/live")).await?;
            Ok(canonical_video_id(&html))
        }
    }
}

async fn post_live_chat(client: &reqwest::Client, options: &PageOptions, continuation: &str) -> Result<Value, String> {
    let body = json!({
        "context": {"client": {"clientName": "WEB", "clientVersion": options.client_version, "hl": "ja", "gl": "JP"}},
        "continuation": continuation,
    });
    let response = client
        .post(format!("https://www.youtube.com/youtubei/v1/live_chat/get_live_chat?key={}", options.api_key))
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", &options.client_version)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("live_chat error: {}", response.status()));
    }
    response.json().await.map_err(|e| e.to_string())
}

/// Polls the chat until cancelled or the stream ends. Errors before the first poll stop it;
/// later errors are retried.
pub async fn run(target: Target, cancel: CancellationToken, mut emit: impl FnMut(Event)) -> Result<(), String> {
    let client = client();
    let video_id = resolve_video_id(&client, &target).await?.ok_or("配信中の動画が見つかりません")?;
    let html = get_text(&client, &format!("https://www.youtube.com/watch?v={video_id}")).await?;
    let options = extract_options(&html)?;
    let first = post_live_chat(&client, &options, &options.continuation).await?;
    let mut continuation = all_chat_continuation(&first).unwrap_or_else(|| options.continuation.clone());
    emit(Event::Stream { video_id });

    let mut ended = EndTracker::default();
    while !cancel.is_cancelled() {
        let delay = match post_live_chat(&client, &options, &continuation).await {
            Ok(data) => {
                let poll = parse_poll(&data);
                emit(Event::Batch(poll.batch));
                match poll.next {
                    Some((next, timeout)) => {
                        continuation = next;
                        ended.record(true);
                        emit(Event::Status("running"));
                        Duration::from_millis(timeout.clamp(500, 10_000))
                    }
                    None => {
                        if ended.record(false) {
                            return Err("配信が終了したため、チャットの取得を停止しました".into());
                        }
                        emit(Event::Status("reconnecting"));
                        RETRY_DELAY
                    }
                }
            }
            Err(_) => {
                emit(Event::Status("reconnecting"));
                RETRY_DELAY
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str) -> Result<Target, String> {
        Ok(Target::Video(id.into()))
    }

    #[test]
    fn accepts_urls_and_the_existing_inputs() {
        for input in [
            "https://www.youtube.com/watch?v=abc_DEF-123",
            "https://www.youtube.com/watch?list=PL1&v=abc_DEF-123&t=10s",
            "https://m.youtube.com/watch?v=abc_DEF-123",
            "youtube.com/watch?v=abc_DEF-123",
            "https://youtu.be/abc_DEF-123?si=share",
            "youtu.be/abc_DEF-123",
            "https://www.youtube.com/live/abc_DEF-123?feature=share",
            "https://www.youtube.com/shorts/abc_DEF-123",
            "https://www.youtube.com/live_chat?is_popout=1&v=abc_DEF-123",
            "  abc_DEF-123  ",
        ] {
            assert_eq!(parse_target(input), video("abc_DEF-123"), "{input}");
        }
        assert_eq!(parse_target("https://www.youtube.com/@yui/live"), Ok(Target::LivePath("/@yui".into())));
        assert_eq!(parse_target("@yui"), Ok(Target::LivePath("/@yui".into())));
        assert_eq!(parse_target("https://www.youtube.com/channel/UCabc/streams"), Ok(Target::LivePath("/channel/UCabc".into())));
        assert_eq!(parse_target("https://www.youtube.com/c/yui"), Ok(Target::LivePath("/c/yui".into())));
        assert_eq!(parse_target("UCxxxxxxxxxxxxxxxxxxxxxx"), Ok(Target::LivePath("/channel/UCxxxxxxxxxxxxxxxxxxxxxx".into())));
    }

    #[test]
    fn explains_urls_that_cannot_lead_to_a_stream() {
        for (input, message) in [
            ("https://example.com/watch?v=abc_DEF-123", "YouTubeのURL"),
            ("https://www.youtube.com/watch", "動画ID"),
            ("https://www.youtube.com/watch?v=short", "動画ID"),
            ("https://www.youtube.com/feed/subscriptions", "配信を探せません"),
            ("https://", "読み取れません"),
            ("", "空"),
        ] {
            let error = parse_target(input).unwrap_err();
            assert!(error.contains(message), "{input}: {error}");
        }
    }

    #[test]
    fn only_yen_has_an_amount() {
        assert_eq!(parse_yen("¥1,000"), Some(1_000_000_000));
        assert_eq!(parse_yen("￥500"), Some(500_000_000));
        assert_eq!(parse_yen("JP¥2,000"), Some(2_000_000_000));
        for other in ["$5.00", "CA$5.00", "CN¥50", "€1.000,50", ""] {
            assert_eq!(parse_yen(other), None, "{other}");
        }
    }

    #[test]
    fn reads_page_options_and_canonical_video() {
        let html = r#""INNERTUBE_API_KEY":"key" "clientVersion":"2.2026" "continuation":"first" <link rel="canonical" href="https://www.youtube.com/watch?v=abc_DEF-123">"#;
        assert_eq!(
            extract_options(html),
            Ok(PageOptions { api_key: "key".into(), client_version: "2.2026".into(), continuation: "first".into() })
        );
        assert_eq!(canonical_video_id(html).as_deref(), Some("abc_DEF-123"));
        assert!(extract_options("<html></html>").is_err());
    }

    fn poll(actions: Value) -> Poll {
        parse_poll(&json!({"continuationContents": {"liveChatContinuation": {
            "actions": actions,
            "continuations": [{"timedContinuationData": {"continuation": "next", "timeoutMs": 1500}}]
        }}}))
    }

    fn text_action(id: &str, channel: &str, runs: Value) -> Value {
        json!({"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {
            "id": id, "authorExternalChannelId": channel, "authorName": {"simpleText": "Viewer"},
            "authorPhoto": {"thumbnails": [{"url": "//yt3.ggpht.com/small"}, {"url": "https://yt3.ggpht.com/large"}]},
            "timestampUsec": "1700000000123456", "message": {"runs": runs},
            "authorBadges": [{"liveChatAuthorBadgeRenderer": {"tooltip": "モデレーター", "icon": {"iconType": "MODERATOR"}}}]
        }}}})
    }

    #[test]
    fn parses_text_with_emoji_badges_and_images() {
        let result = poll(json!([text_action("m1", "UC1", json!([
            {"text": "hi "},
            {"emoji": {"emojiId": "e", "shortcuts": [":wave:"], "image": {"thumbnails": [{"url": "https://yt3.ggpht.com/emoji"}]}}},
            {"emoji": {"emojiId": "bad", "image": {"thumbnails": [{"url": "https://evil.example/x"}]}}}
        ]))]));
        let message = &result.batch.messages[0];
        assert_eq!(message.id, "m1");
        assert_eq!(message.author_id.as_deref(), Some("UC1"));
        assert_eq!(message.created_ms, 1_700_000_000_123);
        assert_eq!(message.icon.as_deref(), Some("https://yt3.ggpht.com/large"));
        assert_eq!(message.text, "hi bad");
        assert_eq!(message.parts[1], Part::Emoji { url: "https://yt3.ggpht.com/emoji".into(), alt: ":wave:".into() });
        assert_eq!(message.parts[2], Part::Text { text: "bad".into() }, "emoji from another host becomes text");
        assert!(message.badges.moderator && !message.badges.owner);
        assert_eq!(result.next, Some(("next".into(), 1500)));
    }

    #[test]
    fn parses_paid_sticker_membership_and_gifts() {
        let result = poll(json!([
            {"addChatItemAction": {"item": {"liveChatPaidMessageRenderer": {
                "id": "paid", "authorName": {"simpleText": "A"}, "timestampUsec": "1000",
                "purchaseAmountText": {"simpleText": "$5.00"}, "bodyBackgroundColor": 4294953512u64,
                "bodyTextColor": 4278190080u64, "message": {"runs": [{"text": "<img src=x>"}]}}}}},
            {"addChatItemAction": {"item": {"liveChatPaidStickerRenderer": {
                "id": "sticker", "authorName": {"simpleText": "B"}, "timestampUsec": "1000",
                "purchaseAmountText": {"simpleText": "¥1,500"}, "backgroundColor": 4280191205u64,
                "sticker": {"thumbnails": [{"url": "https://lh3.googleusercontent.com/s"}], "accessibility": {"accessibilityData": {"label": "cat"}}}}}}},
            {"addChatItemAction": {"item": {"liveChatMembershipItemRenderer": {
                "id": "member", "authorName": {"simpleText": "C"}, "timestampUsec": "1000",
                "headerPrimaryText": {"runs": [{"text": "Member for 12 months"}]}, "headerSubtext": {"runs": [{"text": "Gold"}]},
                "message": {"runs": [{"text": "Thank you!"}]}}}}},
            {"addChatItemAction": {"item": {"liveChatSponsorshipsGiftPurchaseAnnouncementRenderer": {
                "id": "gift", "timestampUsec": "1000",
                "header": {"liveChatSponsorshipsHeaderRenderer": {"authorName": {"simpleText": "@Giver"}}}}}}},
            {"addChatItemAction": {"item": {"liveChatGiftRedemptionAnnouncementRenderer": {
                "id": "redeem", "timestampUsec": "1000", "header": {"runs": [{"text": "Dさんがギフトを受け取りました"}]}}}}}
        ]));
        let [paid, sticker, member, gift, redeem] = &result.batch.messages[..] else { panic!("five messages") };
        assert_eq!(paid.text, "<img src=x>", "markup stays text");
        assert_eq!((paid.amount_micros, paid.amount_text.as_deref()), (None, Some("$5.00")));
        assert_eq!(paid.colors.as_ref().unwrap().body_bg.as_deref(), Some("#FFCA28"));
        assert_eq!((sticker.amount_micros, sticker.currency.as_deref()), (Some(1_500_000_000), Some("JPY")));
        assert_eq!(sticker.parts, vec![Part::Sticker { url: "https://lh3.googleusercontent.com/s".into(), alt: "cat".into() }]);
        assert_eq!(member.text, "Member for 12 months Gold Thank you!");
        assert!(member.badges.member);
        assert_eq!((gift.author.as_str(), gift.text.as_str()), ("Giver", "Giver さんがギフトをくれました！"));
        assert_eq!((redeem.author.as_str(), redeem.kind.as_str()), ("D", kind::GIFT_REDEEM));
    }

    #[test]
    fn reads_every_removal_action() {
        let result = poll(json!([
            {"markChatItemAsDeletedAction": {"targetItemId": "a"}},
            {"removeChatItemAction": {"targetItemId": "b"}},
            {"markChatItemsByAuthorAsDeletedAction": {"externalChannelId": "UC1"}},
            {"removeChatItemByAuthorAction": {"externalChannelId": "UC2"}}
        ]));
        assert_eq!(result.batch.removals, vec![
            Removal::Message("a".into()), Removal::Message("b".into()),
            Removal::Author("UC1".into()), Removal::Author("UC2".into()),
        ]);
    }

    #[test]
    fn a_response_without_continuation_has_no_next() {
        assert_eq!(parse_poll(&json!({"responseContext": {}})).next, None);
        let mut tracker = EndTracker::default();
        assert!(!tracker.record(false));
        assert!(!tracker.record(true), "a single empty response recovers");
        assert!(!tracker.record(false));
        assert!(!tracker.record(false));
        assert!(tracker.record(false), "three in a row end the stream");
    }

    #[test]
    fn switches_to_all_chat() {
        let data = json!({"continuationContents": {"liveChatContinuation": {"header": {"liveChatHeaderRenderer": {"viewSelector": {
            "sortFilterSubMenuRenderer": {"subMenuItems": [
                {"selected": true, "continuation": {"reloadContinuationData": {"continuation": "top"}}},
                {"selected": false, "continuation": {"reloadContinuationData": {"continuation": "all"}}}
            ]}}}}}}});
        assert_eq!(all_chat_continuation(&data).as_deref(), Some("all"));
    }
}
