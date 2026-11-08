//! SQLite storage. Viewers are identified by platform user id; deleted messages keep their row
//! so a re-delivered copy cannot come back (K1, K2, K5).
//!
//! Writes are free functions over `&Connection`, so a transaction (which derefs to a
//! connection) can use them for the one-time import.

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

use crate::model::{message_key, Badges, ChatMessage, Colors, Part, Platform, Removal};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS streams (
  id INTEGER PRIMARY KEY,
  platform TEXT NOT NULL,
  external_id TEXT NOT NULL,
  first_seen_ms INTEGER NOT NULL,
  UNIQUE (platform, external_id)
);
CREATE INDEX IF NOT EXISTS idx_streams_platform_time ON streams(platform, first_seen_ms);

CREATE TABLE IF NOT EXISTS viewers (
  id INTEGER PRIMARY KEY,
  platform TEXT NOT NULL,
  platform_user_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  icon_url TEXT,
  first_seen_ms INTEGER NOT NULL,
  UNIQUE (platform, platform_user_id)
);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY,
  stream_id INTEGER NOT NULL REFERENCES streams(id),
  viewer_id INTEGER REFERENCES viewers(id),
  platform_message_id TEXT NOT NULL,
  author_name TEXT NOT NULL,
  icon_url TEXT,
  kind TEXT NOT NULL,
  text TEXT NOT NULL,
  parts_json TEXT NOT NULL,
  amount_micros INTEGER,
  currency TEXT,
  amount_text TEXT,
  colors_json TEXT,
  badges_json TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  deleted_ms INTEGER,
  UNIQUE (stream_id, platform_message_id)
);
CREATE INDEX IF NOT EXISTS idx_messages_stream_time ON messages(stream_id, created_ms);
CREATE INDEX IF NOT EXISTS idx_messages_viewer_time ON messages(viewer_id, created_ms);
CREATE INDEX IF NOT EXISTS idx_messages_deleted ON messages(deleted_ms) WHERE deleted_ms IS NOT NULL;

CREATE TABLE IF NOT EXISTS participants (
  id INTEGER PRIMARY KEY,
  stream_id INTEGER NOT NULL REFERENCES streams(id),
  message_id INTEGER NOT NULL UNIQUE REFERENCES messages(id),
  viewer_id INTEGER REFERENCES viewers(id),
  status TEXT NOT NULL,
  queue_order INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_participants_stream ON participants(stream_id, status, queue_order);

CREATE TABLE IF NOT EXISTS ohayo (
  stream_id INTEGER PRIMARY KEY REFERENCES streams(id),
  adjustment INTEGER NOT NULL DEFAULT 0,
  reset_ms INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS support_read (
  message_id INTEGER PRIMARY KEY REFERENCES messages(id),
  read_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL
);
"#;

/// Viewers known only by name: comments imported from the Electron version, or messages without an id.
const NAME_PREFIX: &str = "name:";
const LEGACY_IMPORT_KEY: &str = "legacy_import";

pub type Result<T> = std::result::Result<T, String>;

pub(crate) fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// A stored message as the screens see it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Shown {
    pub key: String,
    /// The database row; screens use it to refer back to the message.
    pub row_id: i64,
    #[serde(skip)]
    pub viewer_id: Option<i64>,
    pub stream_id: i64,
    #[serde(flatten)]
    pub message: ChatMessage,
    pub first_time: bool,
    pub last_stream_ms: Option<i64>,
    pub last_stream_ago: Option<i64>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct ImportSummary {
    pub streams: i64,
    pub viewers: i64,
    pub messages: i64,
}

fn ensure_stream(conn: &Connection, platform: Platform, external_id: &str, now_ms: i64) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO streams (platform, external_id, first_seen_ms) VALUES (?1, ?2, ?3)",
        params![platform.as_str(), external_id, now_ms],
    )
    .map_err(err)?;
    conn.query_row(
        "SELECT id FROM streams WHERE platform = ?1 AND external_id = ?2",
        params![platform.as_str(), external_id],
        |r| r.get(0),
    )
    .map_err(err)
}

/// Finds or creates the viewer. A YouTube viewer seen for the first time with a channel id
/// takes over the name-only viewer from imported history, so returning viewers keep their past.
fn viewer_for(conn: &Connection, message: &ChatMessage) -> Result<i64> {
    let platform = message.platform.as_str();
    let user_id = message.author_id.clone().unwrap_or_else(|| format!("{NAME_PREFIX}{}", message.author));
    let found: Option<i64> = conn
        .query_row(
            "SELECT id FROM viewers WHERE platform = ?1 AND platform_user_id = ?2",
            params![platform, user_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    if let Some(id) = found {
        conn.execute(
            "UPDATE viewers SET display_name = ?2, icon_url = COALESCE(?3, icon_url), first_seen_ms = MIN(first_seen_ms, ?4) WHERE id = ?1",
            params![id, message.author, message.icon, message.created_ms],
        )
        .map_err(err)?;
        return Ok(id);
    }
    if message.author_id.is_some() && message.platform == Platform::Youtube {
        let linked = conn
            .execute(
                "UPDATE viewers SET platform_user_id = ?1 WHERE platform = 'youtube' AND platform_user_id = ?2",
                params![user_id, format!("{NAME_PREFIX}{}", message.author)],
            )
            .map_err(err)?;
        if linked > 0 {
            return viewer_for(conn, message);
        }
    }
    conn.execute(
        "INSERT INTO viewers (platform, platform_user_id, display_name, icon_url, first_seen_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![platform, user_id, message.author, message.icon, message.created_ms],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

/// Returns the new row id, or `None` when the message was already stored.
fn insert_row(conn: &Connection, stream_id: i64, viewer_id: i64, message: &ChatMessage, deleted_ms: Option<i64>) -> Result<Option<i64>> {
    let changed = conn
        .execute(
            "INSERT OR IGNORE INTO messages (stream_id, viewer_id, platform_message_id, author_name, icon_url, kind, text, parts_json,
               amount_micros, currency, amount_text, colors_json, badges_json, created_ms, deleted_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                stream_id,
                viewer_id,
                message.id,
                message.author,
                message.icon,
                message.kind,
                message.text,
                serde_json::to_string(&message.parts).unwrap_or_else(|_| "[]".into()),
                message.amount_micros,
                message.currency,
                message.amount_text,
                message.colors.as_ref().and_then(|c| serde_json::to_string(c).ok()),
                serde_json::to_string(&message.badges).unwrap_or_else(|_| "{}".into()),
                message.created_ms,
                deleted_ms,
            ],
        )
        .map_err(err)?;
    Ok((changed > 0).then(|| conn.last_insert_rowid()))
}

fn set_setting(conn: &Connection, key: &str, value: &serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value_json) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        params![key, value.to_string()],
    )
    .map(|_| ())
    .map_err(err)
}

fn count(conn: &Connection, table: &str) -> Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).map_err(err)
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        Self::init(Connection::open(path).map_err(err)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(err)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").map_err(err)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(err)?;
        conn.execute_batch(SCHEMA).map_err(err)?;
        Ok(Self { conn })
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn ensure_stream(&self, platform: Platform, external_id: &str, now_ms: i64) -> Result<i64> {
        ensure_stream(&self.conn, platform, external_id, now_ms)
    }

    fn stream_platform(&self, stream_id: i64) -> Result<Platform> {
        let name: String = self
            .conn
            .query_row("SELECT platform FROM streams WHERE id = ?1", [stream_id], |r| r.get(0))
            .map_err(err)?;
        Platform::parse(&name).ok_or_else(|| format!("unknown platform {name}"))
    }

    /// Stores a message. Returns `None` for a message already stored, including deleted ones (K2).
    pub fn insert(&self, stream_id: i64, message: &ChatMessage) -> Result<Option<Shown>> {
        let viewer_id = viewer_for(&self.conn, message)?;
        match insert_row(&self.conn, stream_id, viewer_id, message, None)? {
            Some(row_id) => Ok(Some(self.shown(stream_id, viewer_id, row_id, message.clone())?)),
            None => Ok(None),
        }
    }

    fn shown(&self, stream_id: i64, viewer_id: i64, row_id: i64, message: ChatMessage) -> Result<Shown> {
        let created = message.created_ms;
        let first_time: bool = self
            .conn
            .query_row(
                "SELECT NOT EXISTS (SELECT 1 FROM messages WHERE viewer_id = ?1 AND (created_ms < ?2 OR (created_ms = ?2 AND id < ?3)))",
                params![viewer_id, created, row_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        let mut last_stream_ms = None;
        let mut last_stream_ago = None;
        if !first_time {
            let previous: Option<(String, i64, i64)> = self
                .conn
                .query_row(
                    "SELECT cur.platform, cur.first_seen_ms, prev.first_seen_ms
                     FROM streams cur, messages m JOIN streams prev ON prev.id = m.stream_id
                     WHERE cur.id = ?2 AND m.viewer_id = ?1 AND m.stream_id <> ?2 AND prev.first_seen_ms < cur.first_seen_ms
                     ORDER BY m.created_ms DESC LIMIT 1",
                    params![viewer_id, stream_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(err)?;
            if let Some((platform, current, prev)) = previous {
                last_stream_ms = Some(prev);
                last_stream_ago = Some(
                    self.conn
                        .query_row(
                            "SELECT COUNT(*) FROM streams WHERE platform = ?1 AND first_seen_ms > ?2 AND first_seen_ms <= ?3",
                            params![platform, prev, current],
                            |r| r.get(0),
                        )
                        .map_err(err)?,
                );
            }
        }
        Ok(Shown {
            key: message.key(),
            row_id,
            viewer_id: Some(viewer_id),
            stream_id,
            message,
            first_time,
            last_stream_ms,
            last_stream_ago,
        })
    }

    /// Marks messages deleted and returns their keys (K1).
    pub fn apply_removal(&self, stream_id: i64, removal: &Removal, now_ms: i64) -> Result<Vec<String>> {
        let platform = self.stream_platform(stream_id)?;
        let (filter, arg) = match removal {
            Removal::Message(id) => ("platform_message_id = ?2", Some(id.as_str())),
            Removal::Author(user) => (
                "viewer_id = (SELECT id FROM viewers WHERE platform = ?3 AND platform_user_id = ?2)",
                Some(user.as_str()),
            ),
            Removal::All => ("?2 IS NULL", None),
        };
        let condition = format!("stream_id = ?1 AND deleted_ms IS NULL AND {filter} AND ?3 = ?3");
        let ids: Vec<String> = {
            let mut statement = self
                .conn
                .prepare(&format!("SELECT platform_message_id FROM messages WHERE {condition}"))
                .map_err(err)?;
            let rows = statement
                .query_map(params![stream_id, arg, platform.as_str()], |r| r.get(0))
                .map_err(err)?;
            rows.collect::<std::result::Result<_, _>>().map_err(err)?
        };
        self.conn
            .execute(
                &format!("UPDATE messages SET deleted_ms = ?4 WHERE {condition}"),
                params![stream_id, arg, platform.as_str(), now_ms],
            )
            .map_err(err)?;
        Ok(ids.iter().map(|id| message_key(platform, id)).collect())
    }

    /// Newest messages of the given streams, oldest first, without deleted ones.
    pub fn recent(&self, stream_ids: &[i64], limit: usize) -> Result<Vec<Shown>> {
        self.select_shown(stream_ids, "", limit)
    }

    /// Like `recent`, with an extra SQL condition on `m` (the messages table).
    pub(crate) fn select_shown(&self, stream_ids: &[i64], extra: &str, limit: usize) -> Result<Vec<Shown>> {
        if stream_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; stream_ids.len()].join(",");
        let sql = format!(
            "SELECT m.id, m.stream_id, m.viewer_id, s.platform, m.platform_message_id, v.platform_user_id, m.author_name, m.icon_url,
                    m.kind, m.text, m.parts_json, m.amount_micros, m.currency, m.amount_text, m.colors_json, m.badges_json, m.created_ms
             FROM messages m JOIN streams s ON s.id = m.stream_id LEFT JOIN viewers v ON v.id = m.viewer_id
             WHERE m.stream_id IN ({placeholders}) AND m.deleted_ms IS NULL {extra}
             ORDER BY m.created_ms DESC, m.id DESC LIMIT {limit}"
        );
        let mut statement = self.conn.prepare(&sql).map_err(err)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(stream_ids), |r| {
                let platform: String = r.get(3)?;
                let user_id: Option<String> = r.get(5)?;
                let parts: String = r.get(10)?;
                let colors: Option<String> = r.get(14)?;
                let badges: String = r.get(15)?;
                let message = ChatMessage {
                    platform: Platform::parse(&platform).unwrap_or(Platform::Youtube),
                    id: r.get(4)?,
                    author_id: user_id.filter(|u| !u.starts_with(NAME_PREFIX)),
                    author: r.get(6)?,
                    icon: r.get(7)?,
                    kind: r.get(8)?,
                    text: r.get(9)?,
                    parts: serde_json::from_str(&parts).unwrap_or_default(),
                    amount_micros: r.get(11)?,
                    currency: r.get(12)?,
                    amount_text: r.get(13)?,
                    colors: colors.and_then(|c| serde_json::from_str::<Colors>(&c).ok()),
                    badges: serde_json::from_str::<Badges>(&badges).unwrap_or_default(),
                    created_ms: r.get(16)?,
                };
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, message))
            })
            .map_err(err)?;
        let rows: Vec<_> = rows.collect::<std::result::Result<_, _>>().map_err(err)?;
        let mut shown = rows
            .into_iter()
            .map(|(row_id, stream_id, viewer_id, message)| match viewer_id {
                Some(viewer_id) => self.shown(stream_id, viewer_id, row_id, message),
                None => Ok(Shown {
                    key: message.key(),
                    row_id,
                    viewer_id: None,
                    stream_id,
                    message,
                    first_time: false,
                    last_stream_ms: None,
                    last_stream_ago: None,
                }),
            })
            .collect::<Result<Vec<_>>>()?;
        shown.reverse();
        Ok(shown)
    }

    pub fn setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value_json FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(err)?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        set_setting(&self.conn, key, value)
    }

    pub fn legacy_imported(&self) -> Result<bool> {
        Ok(self.setting(LEGACY_IMPORT_KEY)?.is_some())
    }

    /// Copies history from the Electron version's comments.db once. The source is opened read-only.
    pub fn import_legacy(&mut self, source: &Path) -> Result<ImportSummary> {
        if self.legacy_imported()? {
            return Err("以前の履歴はもう取り込み済みです".into());
        }
        let old = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|e| format!("以前のデータベースを開けませんでした: {e}"))?;
        let columns: Vec<String> = {
            let mut statement = old.prepare("SELECT name FROM pragma_table_info('comments')").map_err(err)?;
            let rows = statement.query_map([], |r| r.get(0)).map_err(err)?;
            rows.collect::<std::result::Result<_, _>>().map_err(err)?
        };
        if columns.is_empty() {
            return Err("コメントの履歴が見つかりませんでした".into());
        }
        let optional = |name: &str| if columns.iter().any(|c| c == name) { name.to_string() } else { "NULL".to_string() };
        let sql = format!(
            "SELECT id, video_id, timestamp_ms, author, text, kind, amount_text, icon, parts_json, colors_json,
                    is_member, is_moderator, is_owner, {}, {}
             FROM comments WHERE video_id IS NOT NULL AND video_id <> '' AND video_id NOT LIKE 'twitch:%'
             ORDER BY timestamp_ms, rowid",
            optional("author_channel_id"),
            optional("deleted_ms"),
        );

        let tx = self.conn.transaction().map_err(err)?;
        let mut summary = ImportSummary::default();
        {
            let mut statement = old.prepare(&sql).map_err(err)?;
            let mut rows = statement.query([]).map_err(err)?;
            while let Some(r) = rows.next().map_err(err)? {
                let created_ms: i64 = r.get::<_, Option<i64>>(2).map_err(err)?.unwrap_or(0);
                let video_id: String = r.get(1).map_err(err)?;
                let text: String = r.get::<_, Option<String>>(4).map_err(err)?.unwrap_or_default();
                let amount_text: Option<String> = r.get(6).map_err(err)?;
                let amount_micros = amount_text.as_deref().and_then(crate::youtube::parse_yen);
                let parts = r
                    .get::<_, Option<String>>(8)
                    .map_err(err)?
                    .and_then(|p| serde_json::from_str::<Vec<Part>>(&p).ok())
                    .unwrap_or_else(|| vec![Part::Text { text: text.clone() }]);
                let message = ChatMessage {
                    platform: Platform::Youtube,
                    id: r.get(0).map_err(err)?,
                    author_id: r.get::<_, Option<String>>(13).map_err(err)?.filter(|s| !s.is_empty()),
                    author: r.get::<_, Option<String>>(3).map_err(err)?.unwrap_or_else(|| "？？？".into()),
                    icon: r.get::<_, Option<String>>(7).map_err(err)?.and_then(|u| crate::model::allowed_image(&u)),
                    kind: r.get::<_, Option<String>>(5).map_err(err)?.unwrap_or_else(|| "text".into()),
                    text,
                    parts,
                    currency: amount_micros.map(|_| "JPY".into()),
                    amount_micros,
                    amount_text,
                    colors: r.get::<_, Option<String>>(9).map_err(err)?.and_then(|c| serde_json::from_str(&c).ok()),
                    badges: Badges {
                        member: r.get::<_, Option<i64>>(10).map_err(err)? == Some(1),
                        moderator: r.get::<_, Option<i64>>(11).map_err(err)? == Some(1),
                        owner: r.get::<_, Option<i64>>(12).map_err(err)? == Some(1),
                    },
                    created_ms,
                };
                let deleted_ms: Option<i64> = r.get(14).map_err(err)?;
                let stream_id = ensure_stream(&tx, Platform::Youtube, &video_id, created_ms)?;
                let viewer_id = viewer_for(&tx, &message)?;
                if insert_row(&tx, stream_id, viewer_id, &message, deleted_ms)?.is_some() {
                    summary.messages += 1;
                }
            }
        }
        summary.streams = count(&tx, "streams")?;
        summary.viewers = count(&tx, "viewers")?;
        set_setting(
            &tx,
            LEGACY_IMPORT_KEY,
            &serde_json::json!({"source": source.display().to_string(), "messages": summary.messages}),
        )?;
        tx.commit().map_err(err)?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::kind;

    fn message(id: &str, author: &str, author_id: Option<&str>, created_ms: i64) -> ChatMessage {
        ChatMessage {
            platform: Platform::Youtube,
            id: id.into(),
            author_id: author_id.map(str::to_string),
            author: author.into(),
            icon: None,
            kind: kind::TEXT.into(),
            text: format!("text {id}"),
            parts: vec![Part::Text { text: format!("text {id}") }],
            amount_micros: None,
            currency: None,
            amount_text: None,
            colors: None,
            badges: Badges::default(),
            created_ms,
        }
    }

    #[test]
    fn deleted_messages_disappear_and_stay_gone_k1_k2() {
        let db = Db::open_in_memory().unwrap();
        let stream = db.ensure_stream(Platform::Youtube, "v1", 1).unwrap();
        for (id, author, uid, t) in [("a", "A", "UC1", 10), ("b", "Spam", "UC2", 11), ("c", "Spam", "UC2", 12), ("d", "B", "UC3", 13)] {
            db.insert(stream, &message(id, author, Some(uid), t)).unwrap().unwrap();
        }
        assert_eq!(db.apply_removal(stream, &Removal::Message("a".into()), 100).unwrap(), vec!["youtube:a"]);
        let mut by_author = db.apply_removal(stream, &Removal::Author("UC2".into()), 100).unwrap();
        by_author.sort();
        assert_eq!(by_author, vec!["youtube:b", "youtube:c"]);
        assert_eq!(db.apply_removal(stream, &Removal::Message("a".into()), 200).unwrap(), Vec::<String>::new());
        assert_eq!(db.insert(stream, &message("a", "A", Some("UC1"), 10)).unwrap(), None, "re-delivered copy stays hidden");
        let keys: Vec<_> = db.recent(&[stream], 50).unwrap().into_iter().map(|s| s.key).collect();
        assert_eq!(keys, vec!["youtube:d"]);
    }

    #[test]
    fn clear_all_hides_the_whole_stream_only() {
        let db = Db::open_in_memory().unwrap();
        let one = db.ensure_stream(Platform::Twitch, "chan:1", 1).unwrap();
        let two = db.ensure_stream(Platform::Twitch, "other:1", 1).unwrap();
        let mut m = message("x", "A", Some("1"), 5);
        m.platform = Platform::Twitch;
        db.insert(one, &m).unwrap();
        db.insert(two, &ChatMessage { id: "y".into(), ..m.clone() }).unwrap();
        assert_eq!(db.apply_removal(one, &Removal::All, 9).unwrap(), vec!["twitch:x"]);
        assert_eq!(db.recent(&[one, two], 10).unwrap().len(), 1);
    }

    #[test]
    fn viewers_are_told_apart_by_id_not_name_k5() {
        let db = Db::open_in_memory().unwrap();
        let first = db.ensure_stream(Platform::Youtube, "v1", 100).unwrap();
        db.ensure_stream(Platform::Youtube, "v2", 200).unwrap();
        let third = db.ensure_stream(Platform::Youtube, "v3", 300).unwrap();
        db.insert(first, &message("1", "Same", Some("UC1"), 110)).unwrap();
        let renamed = db.insert(third, &message("2", "New Name", Some("UC1"), 310)).unwrap().unwrap();
        let other = db.insert(third, &message("3", "Same", Some("UC2"), 311)).unwrap().unwrap();
        let again = db.insert(third, &message("4", "New Name", Some("UC1"), 312)).unwrap().unwrap();
        assert!(!renamed.first_time, "a renamed viewer is the same person");
        assert_eq!((renamed.last_stream_ms, renamed.last_stream_ago), (Some(100), Some(2)));
        assert!(other.first_time, "same name on another channel is another viewer");
        assert!(!again.first_time);
    }

    fn legacy_db(path: &Path, with_channel: bool) {
        let conn = Connection::open(path).unwrap();
        let channel = if with_channel { ", author_channel_id TEXT, deleted_ms INTEGER" } else { "" };
        conn.execute_batch(&format!(
            "CREATE TABLE comments (id TEXT PRIMARY KEY, video_id TEXT, timestamp_ms INTEGER, timestamp TEXT, author TEXT, text TEXT,
             kind TEXT, amount INTEGER, amount_text TEXT, icon TEXT, parts_json TEXT, colors_json TEXT, is_member INTEGER,
             is_moderator INTEGER, is_owner INTEGER{channel});"
        ))
        .unwrap();
        let rows = [("old1", "vid-old", 1000, "Regular", "hello"), ("old2", "vid-old", 1001, "Once", "hi"), ("tw", "twitch:chan", 1002, "T", "x")];
        for (id, video, t, author, text) in rows {
            conn.execute(
                "INSERT INTO comments (id, video_id, timestamp_ms, author, text, kind, amount_text, parts_json) VALUES (?1, ?2, ?3, ?4, ?5, 'text', '$5.00', ?6)",
                params![id, video, t, author, text, format!(r#"[{{"type":"text","text":"{text}"}}]"#)],
            )
            .unwrap();
        }
    }

/// A database path in its own temporary folder, removed when the test ends.
    /// Declared before the connections that use it, so it is dropped after them.
    struct TempDb(std::path::PathBuf);

    impl std::ops::Deref for TempDb {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDb {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn temp_path(name: &str) -> TempDb {
        let dir = std::env::temp_dir().join(format!("yuilivekit-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDb(dir.join("comments.db"))
    }

    #[test]
    fn imported_history_keeps_returning_viewers_and_runs_once() {
        let source = temp_path("legacy");
        legacy_db(&source, false);
        let before = std::fs::read(&source).unwrap();
        let mut db = Db::open_in_memory().unwrap();
        let summary = db.import_legacy(&source).unwrap();
        assert_eq!(summary, ImportSummary { streams: 1, viewers: 2, messages: 2 }, "Twitch rows are skipped");
        assert_eq!(std::fs::read(&source).unwrap(), before, "the old database is not changed");
        assert!(db.import_legacy(&source).unwrap_err().contains("取り込み済み"));

        let stream = db.ensure_stream(Platform::Youtube, "vid-new", 5000).unwrap();
        let regular = db.insert(stream, &message("n1", "Regular", Some("UCreg"), 5001)).unwrap().unwrap();
        let newcomer = db.insert(stream, &message("n2", "Newcomer", Some("UCnew"), 5002)).unwrap().unwrap();
        assert!(!regular.first_time, "the name-only history joins the channel");
        assert_eq!((regular.last_stream_ms, regular.last_stream_ago), (Some(1000), Some(1)));
        assert!(newcomer.first_time);
        let old = db.ensure_stream(Platform::Youtube, "vid-old", 0).unwrap();
        assert_eq!(db.recent(&[old], 10).unwrap()[0].message.amount_micros, None, "foreign amounts stay text (K8)");
    }

    #[test]
    fn imports_newer_history_with_channel_ids_and_deletions() {
        let source = temp_path("legacy-new");
        legacy_db(&source, true);
        Connection::open(&source)
            .unwrap()
            .execute_batch("UPDATE comments SET author_channel_id = 'UCreg' WHERE author = 'Regular'; UPDATE comments SET deleted_ms = 5 WHERE id = 'old2';")
            .unwrap();
        let mut db = Db::open_in_memory().unwrap();
        db.import_legacy(&source).unwrap();
        let old = db.ensure_stream(Platform::Youtube, "vid-old", 0).unwrap();
        let shown = db.recent(&[old], 10).unwrap();
        assert_eq!(shown.len(), 1, "deleted history stays deleted");
        assert_eq!(shown[0].message.author_id.as_deref(), Some("UCreg"));
    }

    /// K9. Run with `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn queries_stay_fast_with_half_a_million_messages() {
        let path = temp_path("perf");
        let mut db = Db::open(&path).unwrap();
        let tx = db.conn.transaction().unwrap();
        let streams: Vec<i64> = (0..200).map(|s| ensure_stream(&tx, Platform::Youtube, &format!("v{s}"), s * 10_000).unwrap()).collect();
        for i in 0..500_000i64 {
            let stream = streams[(i / 2500) as usize];
            let m = message(&format!("m{i}"), &format!("user{}", i % 20_000), Some(&format!("UC{}", i % 20_000)), (i / 2500) * 10_000 + i % 2500);
            let viewer = viewer_for(&tx, &m).unwrap();
            insert_row(&tx, stream, viewer, &m, None).unwrap();
        }
        tx.commit().unwrap();
        let current = *streams.last().unwrap();
        let started = std::time::Instant::now();
        let shown = db.recent(&[current], 50).unwrap();
        let recent_ms = started.elapsed().as_millis();
        let started = std::time::Instant::now();
        let next = db.insert(current, &message("new", "user7", Some("UC7"), 10_000_000)).unwrap().unwrap();
        let insert_ms = started.elapsed().as_millis();
        println!("recent(50) {recent_ms} ms, insert with history {insert_ms} ms");
        assert_eq!(shown.len(), 50);
        assert!(!next.first_time);
        assert!(recent_ms < 20 && insert_ms < 20, "recent {recent_ms} ms, insert {insert_ms} ms");
    }
}
