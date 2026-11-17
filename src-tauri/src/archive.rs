//! Chat of past streams, read from YouTube's chat replay (`get_live_chat_replay`), so viewers
//! have their history from before the app was collecting.
//!
//! A channel's streams are listed from its "streams" tab and read one at a time, newest first.
//! Requests are spaced out, and wait much longer when YouTube asks to slow down: this is meant
//! to be left running, not to be fast.

use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::model::ChatMessage;
use crate::youtube::{self, Target};

/// The wait before every request.
const STEP: Duration = Duration::from_millis(1500);
/// The waits, in seconds, after YouTube refuses or cannot be reached. Then the run gives up.
const BACKOFF_SECONDS: [u64; 5] = [30, 60, 120, 300, 600];

#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct StreamList {
    pub videos: Vec<Listed>,
    /// For the next page of the list.
    pub next: Option<String>,
}

fn walk(value: &Value, visit: &mut impl FnMut(&Value)) {
    visit(value);
    match value {
        Value::Array(items) => items.iter().for_each(|item| walk(item, visit)),
        Value::Object(map) => map.values().for_each(|item| walk(item, visit)),
        _ => {}
    }
}

/// The JSON a page carries in `var ytInitialData = {...};`.
pub fn initial_data(html: &str) -> Option<Value> {
    let start = html.find("var ytInitialData = ")? + "var ytInitialData = ".len();
    // Reads one JSON value and stops, whatever follows it.
    serde_json::Deserializer::from_str(&html[start..]).into_iter::<Value>().next()?.ok()
}

/// Reads the streams tab, or a later page of it. Videos come in the page's order, newest first.
pub fn parse_stream_list(data: &Value) -> StreamList {
    let mut list = StreamList::default();
    walk(data, &mut |node| {
        let lockup = &node["lockupViewModel"];
        if lockup["contentType"] == "LOCKUP_CONTENT_TYPE_VIDEO" {
            if let Some(id) = lockup["contentId"].as_str() {
                let title = lockup["metadata"]["lockupMetadataViewModel"]["title"]["content"].as_str().unwrap_or("");
                list.videos.push(Listed { id: id.into(), title: title.into() });
            }
        }
        // The list's own "more" item; a page has others, for other things.
        let items = [&node["richGridRenderer"]["contents"], &node["appendContinuationItemsAction"]["continuationItems"]];
        for item in items.iter().filter_map(|i| i.as_array()).flatten() {
            if let Some(token) = item["continuationItemRenderer"]["continuationEndpoint"]["continuationCommand"]["token"].as_str() {
                list.next = Some(token.into());
            }
        }
    });
    list.videos.dedup_by(|a, b| a.id == b.id);
    list
}

/// The chat replay's first continuation, when the page is an archive that has one. A stream
/// that is still on, or ended minutes ago, has none yet.
pub fn replay_token(html: &str) -> Option<String> {
    if !html.contains(r#""isReplay":true"#) {
        return None;
    }
    youtube::capture(html, r#""reloadContinuationData"\s*:\s*\{\s*"continuation"\s*:\s*"([^"]+)""#)
}

/// When the stream went on air, from the page's `startTimestamp`.
pub fn start_time_ms(html: &str) -> Option<i64> {
    let pattern = r#""startTimestamp"\s*:\s*"(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d):(\d\d)(?:\.\d+)?(Z|[+-]\d\d:\d\d)""#;
    let found = regex::Regex::new(pattern).ok()?.captures(html)?;
    let number = |index: usize| found[index].parse::<i64>().ok();
    let (year, month, day) = (number(1)?, number(2)?, number(3)?);
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468;
    let seconds = days * 86_400 + number(4)? * 3600 + number(5)? * 60 + number(6)?;
    let zone = &found[7];
    let offset = if zone == "Z" {
        0
    } else {
        let minutes = zone[1..3].parse::<i64>().ok()? * 60 + zone[4..6].parse::<i64>().ok()?;
        if zone.starts_with('-') { -minutes } else { minutes }
    };
    Some((seconds - offset * 60) * 1000)
}

fn page_title(html: &str) -> String {
    let title = youtube::capture(html, r#"<meta name="title" content="([^"]*)""#).unwrap_or_default();
    [("&quot;", "\""), ("&#39;", "'"), ("&lt;", "<"), ("&gt;", ">"), ("&amp;", "&")]
        .iter()
        .fold(title, |text, (from, to)| text.replace(from, to))
}

#[derive(Debug, Default, PartialEq)]
pub struct ReplayPage {
    pub messages: Vec<ChatMessage>,
    /// `None` at the end of the replay.
    pub next: Option<String>,
}

pub fn parse_replay(data: &Value) -> ReplayPage {
    let live = &data["continuationContents"]["liveChatContinuation"];
    let mut page = ReplayPage::default();
    for (index, action) in live["actions"].as_array().into_iter().flatten().enumerate() {
        for inner in action["replayChatItemAction"]["actions"].as_array().into_iter().flatten() {
            // Notices from YouTube itself carry the time of the request, not of the stream.
            if let Some(message) = youtube::parse_item(&inner["addChatItemAction"]["item"], index).filter(|m| m.created_ms > 0) {
                page.messages.push(message);
            }
        }
    }
    page.next = live["continuations"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|c| c["liveChatReplayContinuationData"]["continuation"].as_str().map(str::to_string));
    page
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// The whole replay was read.
    Done,
    /// The video has no chat replay (not yet, or never).
    NoReplay,
    /// It could not be read this time; it is tried again on the next run.
    Unreadable,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// How many streams the channel lists.
    Listed { total: usize },
    /// A stream that needs nothing: it was read before.
    Skipped,
    Started { title: String },
    Page { video_id: String, started_ms: Option<i64>, messages: Vec<ChatMessage> },
    Finished { video_id: String, title: String, outcome: Outcome },
    /// YouTube asked to slow down (or could not be reached); the next try is this far away.
    Waiting { seconds: u64 },
}

enum Stop {
    Cancelled,
    Failed(String),
}

enum Request<'a> {
    Get(&'a str),
    Post(&'a str, &'a Value),
}

/// A request YouTube may refuse for good (`None`: this one thing cannot be read), after the
/// usual wait. Being refused for now is waited out.
async fn fetch(
    client: &reqwest::Client,
    cancel: &CancellationToken,
    emit: &mut impl FnMut(Event),
    request: Request<'_>,
) -> Result<Option<String>, Stop> {
    let pause = |time: Duration| async move {
        tokio::select! {
            _ = cancel.cancelled() => Err(Stop::Cancelled),
            _ = tokio::time::sleep(time) => Ok(()),
        }
    };
    let mut waits = BACKOFF_SECONDS.iter();
    loop {
        pause(STEP).await?;
        let sent = match &request {
            Request::Get(url) => client.get(*url).header("Accept-Language", "ja").send().await,
            Request::Post(url, body) => client.post(*url).json(body).send().await,
        };
        let problem = match sent {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    match response.text().await {
                        Ok(text) => return Ok(Some(text)),
                        Err(error) => error.to_string(),
                    }
                } else if status.as_u16() == 429 || status.as_u16() == 403 || status.is_server_error() {
                    format!("YouTube が {status} を返しました")
                } else {
                    return Ok(None);
                }
            }
            Err(error) => error.to_string(),
        };
        let Some(seconds) = waits.next() else {
            return Err(Stop::Failed(format!("YouTube から読めない状態が続いたため、取り込みを止めました（{problem}）")));
        };
        emit(Event::Waiting { seconds: *seconds });
        pause(Duration::from_secs(*seconds)).await?;
    }
}

struct Api {
    key: String,
    client_version: String,
}

impl Api {
    fn from_page(html: &str) -> Option<Self> {
        Some(Self {
            key: youtube::capture(html, r#""INNERTUBE_API_KEY"\s*:\s*"([^"]+)""#)?,
            client_version: youtube::capture(html, r#""clientVersion"\s*:\s*"([\d.]+)""#)?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("https://www.youtube.com/youtubei/v1/{path}?key={}", self.key)
    }

    fn body(&self, continuation: &str) -> Value {
        json!({
            "context": {"client": {"clientName": "WEB", "clientVersion": self.client_version, "hl": "ja", "gl": "JP"}},
            "continuation": continuation,
        })
    }
}

async fn list_streams(
    client: &reqwest::Client,
    cancel: &CancellationToken,
    emit: &mut impl FnMut(Event),
    path: &str,
) -> Result<Vec<Listed>, Stop> {
    let not_found = || Stop::Failed("チャンネルの配信一覧を読めませんでした。入力を確かめてください".into());
    let html = fetch(client, cancel, emit, Request::Get(&format!("https://www.youtube.com{path}/streams"))).await?.ok_or_else(not_found)?;
    let api = Api::from_page(&html).ok_or_else(not_found)?;
    let mut page = initial_data(&html).map(|data| parse_stream_list(&data)).unwrap_or_default();
    let mut videos = std::mem::take(&mut page.videos);
    while let Some(token) = page.next.take() {
        emit(Event::Listed { total: videos.len() });
        let Some(text) = fetch(client, cancel, emit, Request::Post(&api.url("browse"), &api.body(&token))).await? else { break };
        page = serde_json::from_str::<Value>(&text).map(|data| parse_stream_list(&data)).unwrap_or_default();
        if page.videos.is_empty() {
            break;
        }
        videos.append(&mut page.videos);
    }
    let mut seen = std::collections::HashSet::new();
    videos.retain(|video| seen.insert(video.id.clone()));
    Ok(videos)
}

async fn read_video(
    client: &reqwest::Client,
    cancel: &CancellationToken,
    emit: &mut impl FnMut(Event),
    video: &Listed,
) -> Result<(), Stop> {
    fn finish(emit: &mut impl FnMut(Event), video: &Listed, title: &str, outcome: Outcome) -> Result<(), Stop> {
        emit(Event::Finished { video_id: video.id.clone(), title: title.to_string(), outcome });
        Ok(())
    }
    let mut title = video.title.clone();
    let url = format!("https://www.youtube.com/watch?v={}", video.id);
    let Some(html) = fetch(client, cancel, emit, Request::Get(&url)).await? else {
        return finish(emit, video, &title, Outcome::NoReplay);
    };
    if title.is_empty() {
        title = page_title(&html);
    }
    let (Some(top), Some(api)) = (replay_token(&html), Api::from_page(&html)) else {
        return finish(emit, video, &title, Outcome::NoReplay);
    };
    let started_ms = start_time_ms(&html);
    emit(Event::Started { title: title.clone() });

    let endpoint = api.url("live_chat/get_live_chat_replay");
    // The page's continuation is "Top chat". Its answer leads to "All chat", which starts over
    // from the beginning; the page's own "All chat" continuation is not accepted here.
    let Some(first) = fetch(client, cancel, emit, Request::Post(&endpoint, &api.body(&top))).await? else {
        return finish(emit, video, &title, Outcome::Unreadable);
    };
    let first: Value = serde_json::from_str(&first).unwrap_or_default();
    let mut token = match youtube::all_chat_continuation(&first) {
        Some(all) => Some(all),
        None => {
            let page = parse_replay(&first);
            emit(Event::Page { video_id: video.id.clone(), started_ms, messages: page.messages });
            page.next
        }
    };
    let mut previous = String::new();
    while let Some(current) = token {
        if current == previous {
            break;
        }
        let Some(text) = fetch(client, cancel, emit, Request::Post(&endpoint, &api.body(&current))).await? else {
            return finish(emit, video, &title, Outcome::Unreadable);
        };
        let page = parse_replay(&serde_json::from_str(&text).unwrap_or_default());
        emit(Event::Page { video_id: video.id.clone(), started_ms, messages: page.messages });
        token = page.next;
        previous = current;
    }
    finish(emit, video, &title, Outcome::Done)
}

/// Reads the chat replay of one video, or of every stream of a channel that `skip` does not
/// turn down, until done or cancelled. Stopping is not an error.
pub async fn run(target: Target, cancel: CancellationToken, skip: impl Fn(&str) -> bool, mut emit: impl FnMut(Event)) -> Result<(), String> {
    let client = youtube::client();
    let result = async {
        let videos = match &target {
            Target::Video(id) => vec![Listed { id: id.clone(), title: String::new() }],
            Target::LivePath(path) => list_streams(&client, &cancel, &mut emit, path).await?,
        };
        emit(Event::Listed { total: videos.len() });
        for video in &videos {
            if skip(&video.id) {
                emit(Event::Skipped);
            } else {
                read_video(&client, &cancel, &mut emit, video).await?;
            }
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) | Err(Stop::Cancelled) => Ok(()),
        Err(Stop::Failed(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lockup(id: &str, title: &str) -> Value {
        json!({"richItemRenderer": {"content": {"lockupViewModel": {
            "contentId": id, "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "metadata": {"lockupMetadataViewModel": {"title": {"content": title}}}
        }}}})
    }

    fn more(token: &str) -> Value {
        json!({"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": token}}}})
    }

    #[test]
    fn lists_streams_from_the_tab_and_from_its_later_pages() {
        // The shapes recorded from a real channel on 2026-11-17.
        let html = format!(
            r#"<script nonce="x">var ytInitialData = {};</script><script>var other = {{"a":1}};</script>"#,
            json!({"contents": {"twoColumnBrowseResultsRenderer": {"tabs": [
                {"tabRenderer": {"content": {"richGridRenderer": {"contents": [lockup("aaaaaaaaaaa", "一つ目 };</script>"), lockup("bbbbbbbbbbb", "二つ目"), more("NEXT")]}}}},
                {"expandableTabRenderer": {}}
            ]}}, "header": {"other": [more("ABOUT")]}})
        );
        let first = parse_stream_list(&initial_data(&html).unwrap());
        assert_eq!(first.videos.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["aaaaaaaaaaa", "bbbbbbbbbbb"]);
        assert_eq!(first.videos[0].title, "一つ目 };</script>", "text inside the data does not end it");
        assert_eq!(first.next.as_deref(), Some("NEXT"), "the list's own continuation, not another one on the page");

        let later = json!({"onResponseReceivedActions": [{"appendContinuationItemsAction": {"continuationItems": [lockup("ccccccccccc", "三つ目")]}}]});
        let last = parse_stream_list(&later);
        assert_eq!(last.videos, vec![Listed { id: "ccccccccccc".into(), title: "三つ目".into() }]);
        assert_eq!(last.next, None, "the last page has no continuation");
    }

    #[test]
    fn an_archive_has_a_replay_and_a_stream_without_one_does_not() {
        let archive = r#"{"liveChatRenderer":{"continuations":[{"reloadContinuationData":{"continuation":"TOP"}}],"isReplay":true,"header":{"x":[{"reloadContinuationData":{"continuation":"ALL"}}]}}}"#;
        assert_eq!(replay_token(archive).as_deref(), Some("TOP"));
        // On air: the chat is there, but it is not a replay.
        assert_eq!(replay_token(r#"{"liveChatRenderer":{"continuations":[{"reloadContinuationData":{"continuation":"LIVE"}}]}}"#), None);
        // Ended minutes ago: no chat at all yet.
        assert_eq!(replay_token(r#"{"playabilityStatus":{"status":"OK","reason":"このライブ イベントは終了しました。"}}"#), None);
    }

    #[test]
    fn reads_when_a_stream_started() {
        let page = |stamp: &str| format!(r#""liveBroadcastDetails":{{"isLiveNow":false,"startTimestamp":"{stamp}","endTimestamp":"x"}}"#);
        assert_eq!(start_time_ms(&page("2026-07-30T14:35:31+00:00")), Some(1_785_422_131_000));
        assert_eq!(start_time_ms(&page("2026-07-30T23:35:31+09:00")), Some(1_785_422_131_000));
        assert_eq!(start_time_ms(&page("1970-01-01T00:00:00Z")), Some(0));
        assert_eq!(start_time_ms(&page("2024-02-29T12:00:00-08:00")), Some(1_709_236_800_000), "leap day");
        assert_eq!(start_time_ms("no stream here"), None);
    }

    fn text(id: &str, usec: &str) -> Value {
        json!({"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {
            "id": id, "timestampUsec": usec, "authorName": {"simpleText": "@viewer"}, "authorExternalChannelId": "UC1",
            "timestampText": {"simpleText": "0:57"}, "message": {"runs": [{"text": "hello"}]}
        }}}})
    }

    #[test]
    fn reads_a_replay_page_and_knows_the_last_one() {
        let data = json!({"continuationContents": {"liveChatContinuation": {
            "actions": [
                {"replayChatItemAction": {"videoOffsetTimeMsec": "0", "actions": [text("before", "1785422086532889")]}},
                // YouTube's own notice, stamped with the time of the request.
                {"replayChatItemAction": {"videoOffsetTimeMsec": "0", "actions": [{"addChatItemAction": {"item": {"liveChatViewerEngagementMessageRenderer": {"id": "notice", "timestampUsec": "1794877742859000"}}}}]}},
                {"replayChatItemAction": {"videoOffsetTimeMsec": "57834", "actions": [text("during", "1785422189000000"), {"addLiveChatTickerItemAction": {}}]}},
                {"replayChatItemAction": {"videoOffsetTimeMsec": "60000", "actions": [{"addChatItemAction": {"item": {"liveChatPaidMessageRenderer": {
                    "id": "paid", "timestampUsec": "1785422191000000", "authorName": {"simpleText": "@fan"}, "authorExternalChannelId": "UC2",
                    "purchaseAmountText": {"simpleText": "¥500"}, "message": {"runs": [{"text": "thanks"}]}
                }}}}]}}
            ],
            "continuations": [{"liveChatReplayContinuationData": {"continuation": "NEXT"}}, {"playerSeekContinuationData": {"continuation": "SEEK"}}]
        }}});
        let page = parse_replay(&data);
        assert_eq!(page.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["before", "during", "paid"]);
        assert_eq!(page.messages[0].created_ms, 1_785_422_086_532, "the time it was written, not its place in the video");
        assert_eq!(page.messages[0].author_id.as_deref(), Some("UC1"));
        assert_eq!(page.messages[2].amount_micros, Some(500_000_000));
        assert_eq!(page.next.as_deref(), Some("NEXT"));

        let end = json!({"continuationContents": {"liveChatContinuation": {"continuations": [{"playerSeekContinuationData": {"continuation": "SEEK"}}]}}});
        assert_eq!(parse_replay(&end), ReplayPage::default(), "only a seek continuation is left at the end");
    }
}
