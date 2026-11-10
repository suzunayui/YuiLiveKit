//! Stream tools kept in the database: the participant queue, the "good morning" counter and
//! read marks for support messages. Every rule lives here, once (K3, K4).

use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{err, Db, Result, Shown};
use crate::model::kind;

pub const DEFAULT_PARTICIPANT_KEYWORD: &str = "参加希望";
pub const DEFAULT_WITHDRAW_KEYWORD: &str = "辞退";
/// Not a bare "おは": that also matched words such as "おはなし".
pub const DEFAULT_OHAYO_KEYWORDS: [&str; 5] = ["おはよ", "おはー", "おっは", "おはお", "ohayo"];
pub const SUPPORT_KINDS: [&str; 6] =
    [kind::PAID, kind::STICKER, kind::MEMBERSHIP, kind::GIFT_PURCHASE, kind::GIFT_REDEEM, kind::BITS];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Participant {
    pub id: i64,
    pub status: String,
    pub author: String,
    pub icon: Option<String>,
    pub text: String,
    pub platform: String,
    pub created_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Ohayo {
    pub detected: i64,
    pub adjustment: i64,
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearMode {
    All,
    Done,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Support {
    #[serde(flatten)]
    pub shown: Shown,
    pub read: bool,
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

fn now_ms() -> i64 {
    crate::core::now_ms()
}

impl Db {
    /// Adds a participant request from a newly stored message. A repeat request from someone
    /// already waiting is recorded as "duplicate", so it never joins later (K3). Each message is
    /// stored once, so nothing is ever considered twice, even after clearing the queue (K4).
    pub fn withdraw_participant(&self, shown: &Shown) -> Result<bool> {
        // Only the person themself can withdraw, so it needs to know who wrote the comment.
        let Some(viewer) = shown.viewer_id.filter(|_| shown.message.kind == kind::TEXT) else { return Ok(false) };
        let changed = self
            .conn()
            .execute(
                "UPDATE participants SET status = 'removed', updated_ms = ?3 WHERE stream_id = ?1 AND viewer_id = ?2 AND status = 'waiting'",
                params![shown.stream_id, viewer, now_ms()],
            )
            .map_err(err)?;
        Ok(changed > 0)
    }

    pub fn request_participant(&self, shown: &Shown, keyword: &str) -> Result<bool> {
        let message = &shown.message;
        if message.kind != kind::TEXT || keyword.is_empty() || !message.text.contains(keyword) {
            return Ok(false);
        }
        let conn = self.conn();
        let waiting: bool = match shown.viewer_id {
            Some(viewer) => conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM participants WHERE stream_id = ?1 AND viewer_id = ?2 AND status = 'waiting')",
                    params![shown.stream_id, viewer],
                    |r| r.get(0),
                )
                .map_err(err)?,
            None => false,
        };
        let next_order: i64 = conn
            .query_row("SELECT COALESCE(MAX(queue_order), 0) + 1 FROM participants WHERE stream_id = ?1", [shown.stream_id], |r| {
                r.get(0)
            })
            .map_err(err)?;
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO participants (stream_id, message_id, viewer_id, status, queue_order, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    shown.stream_id,
                    shown.row_id,
                    shown.viewer_id,
                    if waiting { "duplicate" } else { "waiting" },
                    next_order,
                    now_ms()
                ],
            )
            .map_err(err)?;
        Ok(changed > 0 && !waiting)
    }

    /// Waiting and done participants of the given streams, in queue order.
    pub fn participants(&self, stream_ids: &[i64]) -> Result<Vec<Participant>> {
        if stream_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT p.id, p.status, m.author_name, m.icon_url, m.text, s.platform, m.created_ms
             FROM participants p JOIN messages m ON m.id = p.message_id JOIN streams s ON s.id = p.stream_id
             WHERE p.stream_id IN ({}) AND p.status IN ('waiting', 'done')
             ORDER BY p.queue_order, p.id",
            placeholders(stream_ids.len())
        );
        let mut statement = self.conn().prepare(&sql).map_err(err)?;
        let rows = statement
            .query_map(params_from_iter(stream_ids), |r| {
                Ok(Participant {
                    id: r.get(0)?,
                    status: r.get(1)?,
                    author: r.get(2)?,
                    icon: r.get(3)?,
                    text: r.get(4)?,
                    platform: r.get(5)?,
                    created_ms: r.get(6)?,
                })
            })
            .map_err(err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(err)
    }

    pub fn set_participant_status(&self, id: i64, status: &str) -> Result<()> {
        if !["waiting", "done", "removed"].contains(&status) {
            return Err(format!("unknown status {status}"));
        }
        let conn = self.conn();
        if status == "waiting" {
            // Back in the queue only if that viewer has no other waiting request.
            let busy: bool = conn
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM participants o JOIN participants p ON p.id = ?1
                     WHERE o.stream_id = p.stream_id AND o.viewer_id = p.viewer_id AND o.status = 'waiting' AND o.id <> p.id)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if busy {
                return Err("この人はほかの参加希望で待っています".into());
            }
        }
        conn.execute("UPDATE participants SET status = ?2, updated_ms = ?3 WHERE id = ?1", params![id, status, now_ms()])
            .map(|_| ())
            .map_err(err)
    }

    pub fn clear_participants(&self, stream_ids: &[i64], mode: &ClearMode) -> Result<()> {
        if stream_ids.is_empty() {
            return Ok(());
        }
        let statuses = match mode {
            ClearMode::All => "('waiting', 'done')",
            ClearMode::Done => "('done')",
        };
        let sql = format!(
            "UPDATE participants SET status = 'removed', updated_ms = ? WHERE status IN {statuses} AND stream_id IN ({})",
            placeholders(stream_ids.len())
        );
        let mut values: Vec<i64> = vec![now_ms()];
        values.extend_from_slice(stream_ids);
        self.conn().execute(&sql, params_from_iter(values)).map(|_| ()).map_err(err)
    }

    /// Puts the given participants first, in that order; the rest keep their relative order.
    pub fn reorder_participants(&self, stream_ids: &[i64], ids: &[i64]) -> Result<()> {
        let current: Vec<i64> = self.participants(stream_ids)?.into_iter().map(|p| p.id).collect();
        let mut order: Vec<i64> = ids.iter().copied().filter(|id| current.contains(id)).collect();
        let rest: Vec<i64> = current.iter().copied().filter(|id| !order.contains(id)).collect();
        order.extend(rest);
        let conn = self.conn();
        for (position, id) in order.iter().enumerate() {
            conn.execute("UPDATE participants SET queue_order = ?2 WHERE id = ?1", params![id, position as i64 + 1])
                .map_err(err)?;
        }
        Ok(())
    }

    fn ohayo_row(&self, stream_id: i64) -> Result<(i64, i64)> {
        Ok(self
            .conn()
            .query_row("SELECT adjustment, reset_ms FROM ohayo WHERE stream_id = ?1", [stream_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(err)?
            .unwrap_or((0, 0)))
    }

    /// People who greeted since the last reset, one per person and stream, plus manual changes.
    pub fn ohayo(&self, stream_ids: &[i64], keywords: &[String]) -> Result<Ohayo> {
        let keywords: Vec<&String> = keywords.iter().filter(|k| !k.trim().is_empty()).collect();
        let mut total = Ohayo::default();
        for &stream_id in stream_ids {
            let (adjustment, reset_ms) = self.ohayo_row(stream_id)?;
            total.adjustment += adjustment;
            if keywords.is_empty() {
                continue;
            }
            let matches = vec!["instr(text, ?) > 0"; keywords.len()].join(" OR ");
            let sql = format!(
                "SELECT COUNT(DISTINCT COALESCE(viewer_id, -id)) FROM messages
                 WHERE stream_id = ? AND created_ms > ? AND deleted_ms IS NULL AND kind = 'text' AND ({matches})"
            );
            let mut values: Vec<rusqlite::types::Value> = vec![stream_id.into(), reset_ms.into()];
            values.extend(keywords.iter().map(|k| rusqlite::types::Value::Text(k.to_string())));
            total.detected += self.conn().query_row(&sql, params_from_iter(values), |r| r.get::<_, i64>(0)).map_err(err)?;
        }
        total.count = (total.detected + total.adjustment).max(0);
        Ok(total)
    }

    /// Changes the count by hand. The change is kept on the first stream; the total never drops below zero.
    pub fn adjust_ohayo(&self, stream_ids: &[i64], keywords: &[String], delta: i64) -> Result<Ohayo> {
        let Some(&first) = stream_ids.first() else { return self.ohayo(stream_ids, keywords) };
        let current = self.ohayo(stream_ids, keywords)?;
        let delta = delta.max(-current.count);
        let (adjustment, reset_ms) = self.ohayo_row(first)?;
        self.conn()
            .execute(
                "INSERT INTO ohayo (stream_id, adjustment, reset_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(stream_id) DO UPDATE SET adjustment = excluded.adjustment",
                params![first, adjustment + delta, reset_ms],
            )
            .map_err(err)?;
        self.ohayo(stream_ids, keywords)
    }

    /// Starts counting again from now; earlier greetings no longer count.
    pub fn reset_ohayo(&self, stream_ids: &[i64], now_ms: i64) -> Result<()> {
        for &stream_id in stream_ids {
            self.conn()
                .execute(
                    "INSERT INTO ohayo (stream_id, adjustment, reset_ms) VALUES (?1, 0, ?2)
                     ON CONFLICT(stream_id) DO UPDATE SET adjustment = 0, reset_ms = excluded.reset_ms",
                    params![stream_id, now_ms],
                )
                .map_err(err)?;
        }
        Ok(())
    }

    /// Support messages of the given streams with their read marks, oldest first.
    pub fn support(&self, stream_ids: &[i64]) -> Result<Vec<Support>> {
        let kinds = SUPPORT_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        let shown = self.select_shown(stream_ids, &format!("AND m.kind IN ({kinds})"), 10_000)?;
        let mut statement = self.conn().prepare("SELECT 1 FROM support_read WHERE message_id = ?1").map_err(err)?;
        shown
            .into_iter()
            .map(|shown| {
                let read = statement.exists([shown.row_id]).map_err(err)?;
                Ok(Support { shown, read })
            })
            .collect()
    }

    pub fn set_support_read(&self, row_ids: &[i64], read: bool, now_ms: i64) -> Result<()> {
        for row_id in row_ids {
            if read {
                self.conn()
                    .execute("INSERT OR IGNORE INTO support_read (message_id, read_ms) VALUES (?1, ?2)", params![row_id, now_ms])
            } else {
                self.conn().execute("DELETE FROM support_read WHERE message_id = ?1", [row_id])
            }
            .map_err(err)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Emoji {
    pub code: String,
    pub url: Option<String>,
    pub reading: Option<String>,
    pub sound: Option<String>,
    pub volume: f64,
}

impl Db {
    /// Remembers emoji seen in chat, so the screen can offer readings and sounds for them.
    pub fn record_emoji(&self, message: &crate::model::ChatMessage) -> Result<()> {
        for part in &message.parts {
            if let crate::model::Part::Emoji { url, alt } = part {
                if !alt.is_empty() {
                    self.conn()
                        .execute(
                            "INSERT OR IGNORE INTO emoji (code, url, first_seen_ms) VALUES (?1, ?2, ?3)",
                            params![alt, url, message.created_ms],
                        )
                        .map_err(err)?;
                }
            }
        }
        Ok(())
    }

    pub fn emoji(&self) -> Result<Vec<Emoji>> {
        let mut statement = self
            .conn()
            .prepare("SELECT code, url, reading, sound, volume FROM emoji ORDER BY first_seen_ms DESC, code")
            .map_err(err)?;
        let rows = statement
            .query_map([], |r| {
                Ok(Emoji { code: r.get(0)?, url: r.get(1)?, reading: r.get(2)?, sound: r.get(3)?, volume: r.get(4)? })
            })
            .map_err(err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(err)
    }

    /// Changes one emoji's reading, sound or volume; `None` leaves that value as it is.
    pub fn update_emoji(&self, code: &str, reading: Option<&str>, sound: Option<Option<&str>>, volume: Option<f64>) -> Result<()> {
        let conn = self.conn();
        conn.execute("INSERT OR IGNORE INTO emoji (code, first_seen_ms) VALUES (?1, ?2)", params![code, now_ms()])
            .map_err(err)?;
        if let Some(reading) = reading {
            let reading: String = reading.trim().chars().take(30).collect();
            conn.execute("UPDATE emoji SET reading = NULLIF(?2, '') WHERE code = ?1", params![code, reading]).map_err(err)?;
        }
        if let Some(sound) = sound {
            conn.execute("UPDATE emoji SET sound = ?2 WHERE code = ?1", params![code, sound]).map_err(err)?;
        }
        if let Some(volume) = volume {
            let volume = if volume.is_finite() { volume.clamp(0.0, 2.0) } else { 1.0 };
            conn.execute("UPDATE emoji SET volume = ?2 WHERE code = ?1", params![code, volume]).map_err(err)?;
        }
        Ok(())
    }

    pub fn is_deleted(&self, stream_id: i64, platform_message_id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT deleted_ms IS NOT NULL FROM messages WHERE stream_id = ?1 AND platform_message_id = ?2",
                params![stream_id, platform_message_id],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .map_err(err)?
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Badges, ChatMessage, Part, Platform, Removal};

    fn message(id: &str, author_id: &str, text: &str, created_ms: i64) -> ChatMessage {
        ChatMessage {
            platform: Platform::Youtube,
            id: id.into(),
            author_id: Some(author_id.into()),
            author: format!("name-{author_id}"),
            icon: None,
            kind: kind::TEXT.into(),
            text: text.into(),
            parts: vec![Part::Text { text: text.into() }],
            amount_micros: None,
            currency: None,
            amount_text: None,
            colors: None,
            badges: Badges::default(),
            created_ms,
        }
    }

    fn setup() -> (Db, i64) {
        let db = Db::open_in_memory().unwrap();
        let stream = db.ensure_stream(Platform::Youtube, "v", 1).unwrap();
        (db, stream)
    }

    fn request(db: &Db, stream: i64, id: &str, viewer: &str) -> bool {
        let shown = db.insert(stream, &message(id, viewer, "参加希望です", 10)).unwrap().unwrap();
        db.request_participant(&shown, DEFAULT_PARTICIPANT_KEYWORD).unwrap()
    }

    fn statuses(db: &Db, stream: i64) -> Vec<(String, String)> {
        db.participants(&[stream]).unwrap().into_iter().map(|p| (p.author, p.status)).collect()
    }

    #[test]
    fn a_repeat_request_never_joins_later_k3() {
        let (db, stream) = setup();
        assert!(request(&db, stream, "1", "UC1"));
        assert!(!request(&db, stream, "2", "UC1"), "already waiting");
        let first = db.participants(&[stream]).unwrap()[0].id;
        db.set_participant_status(first, "done").unwrap();
        assert_eq!(statuses(&db, stream), vec![("name-UC1".into(), "done".into())], "the repeat stays out");
        assert!(request(&db, stream, "3", "UC1"), "a new comment after being handled joins again");
    }

    #[test]
    fn clearing_keeps_people_out_and_redelivery_changes_nothing_k4() {
        let (db, stream) = setup();
        request(&db, stream, "1", "UC1");
        request(&db, stream, "2", "UC2");
        db.clear_participants(&[stream], &ClearMode::All).unwrap();
        assert!(db.participants(&[stream]).unwrap().is_empty());
        assert_eq!(db.insert(stream, &message("1", "UC1", "参加希望です", 10)).unwrap(), None, "re-read comments are not new");
        assert!(db.participants(&[stream]).unwrap().is_empty());
    }

    #[test]
    fn only_text_with_the_keyword_counts_and_order_can_change() {
        let (db, stream) = setup();
        let plain = db.insert(stream, &message("p", "UC9", "こんにちは", 1)).unwrap().unwrap();
        assert!(!db.request_participant(&plain, DEFAULT_PARTICIPANT_KEYWORD).unwrap());
        for (id, viewer) in [("a", "UC1"), ("b", "UC2"), ("c", "UC3")] {
            request(&db, stream, id, viewer);
        }
        let ids: Vec<i64> = db.participants(&[stream]).unwrap().iter().map(|p| p.id).collect();
        db.reorder_participants(&[stream], &[ids[2]]).unwrap();
        let order: Vec<String> = db.participants(&[stream]).unwrap().into_iter().map(|p| p.author).collect();
        assert_eq!(order, ["name-UC3", "name-UC1", "name-UC2"]);
        db.clear_participants(&[stream], &ClearMode::Done).unwrap();
        assert_eq!(db.participants(&[stream]).unwrap().len(), 3, "clearing done leaves the waiting ones");
    }

    #[test]
    fn waiting_again_is_refused_while_another_request_waits() {
        let (db, stream) = setup();
        request(&db, stream, "1", "UC1");
        let first = db.participants(&[stream]).unwrap()[0].id;
        db.set_participant_status(first, "done").unwrap();
        request(&db, stream, "2", "UC1");
        assert!(db.set_participant_status(first, "waiting").is_err());
    }

    #[test]
    fn ohayo_counts_each_person_once_since_the_reset() {
        let (db, stream) = setup();
        let keywords: Vec<String> = DEFAULT_OHAYO_KEYWORDS.iter().map(|k| k.to_string()).collect();
        for (id, viewer, text, t) in [
            ("1", "UC1", "おはよう！", 10),
            ("2", "UC1", "おはよ", 11),
            ("3", "UC2", "おっはー", 12),
            ("4", "UC3", "おはなしして", 13),
            ("5", "UC4", "こんにちは", 14),
        ] {
            db.insert(stream, &message(id, viewer, text, t)).unwrap();
        }
        assert_eq!(db.ohayo(&[stream], &keywords).unwrap(), Ohayo { detected: 2, adjustment: 0, count: 2 });
        assert_eq!(db.adjust_ohayo(&[stream], &keywords, 3).unwrap().count, 5);
        assert_eq!(db.adjust_ohayo(&[stream], &keywords, -10).unwrap().count, 0, "never below zero");
        db.apply_removal(stream, &Removal::Message("3".into()), 20).unwrap();
        db.reset_ohayo(&[stream], 15).unwrap();
        assert_eq!(db.ohayo(&[stream], &keywords).unwrap().count, 0, "the reset forgets earlier greetings");
        db.insert(stream, &message("6", "UC1", "おはよう", 16)).unwrap();
        assert_eq!(db.ohayo(&[stream], &keywords).unwrap().count, 1);
    }

    #[test]
    fn emoji_are_remembered_with_readings_and_sounds() {
        let (db, stream) = setup();
        let mut m = message("e", "UC1", "hi", 5);
        m.parts.push(Part::Emoji { url: "https://yt3.ggpht.com/e".into(), alt: ":wave:".into() });
        db.insert(stream, &m).unwrap();
        db.record_emoji(&m).unwrap();
        db.record_emoji(&m).unwrap();
        db.update_emoji(":wave:", Some(" ばいばい "), Some(Some("C:/s.wav")), Some(9.0)).unwrap();
        let emoji = db.emoji().unwrap();
        assert_eq!(emoji.len(), 1);
        assert_eq!((emoji[0].reading.as_deref(), emoji[0].sound.as_deref(), emoji[0].volume), (Some("ばいばい"), Some("C:/s.wav"), 2.0));
        db.update_emoji(":wave:", Some(""), Some(None), None).unwrap();
        let emoji = db.emoji().unwrap();
        assert_eq!((emoji[0].reading.as_deref(), emoji[0].sound.as_deref()), (None, None));
        assert!(!db.is_deleted(stream, "e").unwrap());
        db.apply_removal(stream, &Removal::Message("e".into()), 9).unwrap();
        assert!(db.is_deleted(stream, "e").unwrap());
    }

    #[test]
    fn support_messages_keep_read_marks() {
        let (db, stream) = setup();
        let mut paid = message("paid", "UC1", "thanks", 10);
        paid.kind = kind::PAID.into();
        let paid = db.insert(stream, &paid).unwrap().unwrap();
        db.insert(stream, &message("text", "UC2", "hi", 11)).unwrap();
        let support = db.support(&[stream]).unwrap();
        assert_eq!(support.len(), 1);
        assert!(!support[0].read);
        db.set_support_read(&[paid.row_id], true, 20).unwrap();
        assert!(db.support(&[stream]).unwrap()[0].read);
        db.set_support_read(&[paid.row_id], false, 21).unwrap();
        assert!(!db.support(&[stream]).unwrap()[0].read);
    }
}
