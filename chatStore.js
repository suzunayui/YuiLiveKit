// chatStore.js
// Simple SQLite helper for storing live chat messages (better-sqlite3).
const path = require("path");
const fs = require("fs");
const Database = require("better-sqlite3");
const {EventEmitter} = require('node:events');
const commentEvents = new EventEmitter();
function onCommentSaved(listener) {
  commentEvents.on('comment', listener);
  return () => commentEvents.off('comment', listener);
}

let db = null;
let insertStmt = null;
let linkAuthorStmt = null;
let dbPath = null;

const DEFAULT_LIMIT = 50;
const DELETED_ID_LIMIT = 500;

function ensureColorsColumn() {
  try {
    const rows = db.prepare("PRAGMA table_info(comments)").all();
    const hasColors = rows.some((r) => r.name === "colors_json");
    if (!hasColors) {
      db.prepare("ALTER TABLE comments ADD COLUMN colors_json TEXT").run();
    }
  } catch (err) {
    console.warn("ensureColorsColumn error:", err.message || err);
  }
}

function ensureIconColumn() {
  try {
    const rows = db.prepare("PRAGMA table_info(comments)").all();
    const hasIcon = rows.some((r) => r.name === "icon");
    if (!hasIcon) {
      db.prepare("ALTER TABLE comments ADD COLUMN icon TEXT").run();
    }
  } catch (err) {
    console.warn("ensureIconColumn error:", err.message || err);
  }
}

function ensureBadgeColumns() {
  try {
    const rows = db.prepare("PRAGMA table_info(comments)").all();
    const hasMember = rows.some((r) => r.name === "is_member");
    const hasModerator = rows.some((r) => r.name === "is_moderator");
    const hasOwner = rows.some((r) => r.name === "is_owner");
    if (!hasMember) {
      db.prepare("ALTER TABLE comments ADD COLUMN is_member INTEGER").run();
    }
    if (!hasModerator) {
      db.prepare("ALTER TABLE comments ADD COLUMN is_moderator INTEGER").run();
    }
    if (!hasOwner) {
      db.prepare("ALTER TABLE comments ADD COLUMN is_owner INTEGER").run();
    }
  } catch (err) {
    console.warn("ensureBadgeColumns error:", err.message || err);
  }
}

function ensureModerationColumns() {
  try {
    const rows = db.prepare("PRAGMA table_info(comments)").all();
    if (!rows.some((r) => r.name === "author_channel_id")) {
      db.prepare("ALTER TABLE comments ADD COLUMN author_channel_id TEXT").run();
    }
    if (!rows.some((r) => r.name === "deleted_ms")) {
      db.prepare("ALTER TABLE comments ADD COLUMN deleted_ms INTEGER").run();
    }
  } catch (err) {
    console.warn("ensureModerationColumns error:", err.message || err);
  }
}

// Overlays poll several times a second, so these queries must stay fast as history grows.
function ensureIndexes() {
  try {
    db.exec(
      `CREATE INDEX IF NOT EXISTS idx_comments_timestamp ON comments(timestamp_ms);
       CREATE INDEX IF NOT EXISTS idx_comments_author_video ON comments(author, video_id, timestamp_ms);
       CREATE INDEX IF NOT EXISTS idx_comments_video ON comments(video_id, timestamp_ms);
       CREATE INDEX IF NOT EXISTS idx_comments_channel ON comments(author_channel_id, video_id);
       CREATE INDEX IF NOT EXISTS idx_comments_viewer ON comments(COALESCE(author_channel_id, author), video_id, timestamp_ms);
       CREATE INDEX IF NOT EXISTS idx_comments_deleted ON comments(deleted_ms) WHERE deleted_ms IS NOT NULL;`
    );
  } catch (err) {
    console.warn("ensureIndexes error:", err.message || err);
  }
}

function initChatStore(baseDir) {
  // Use a writable dir (e.g. Electron's userData) to avoid ASAR issues.
  const dir = baseDir || process.cwd();
  fs.mkdirSync(dir, { recursive: true });
  dbPath = path.join(dir, "comments.db");

  db = new Database(dbPath);
  try {
    db.pragma("journal_mode = WAL");
  } catch (_) {}

  db.exec(
    `CREATE TABLE IF NOT EXISTS comments (
      id TEXT PRIMARY KEY,
      video_id TEXT,
      timestamp_ms INTEGER,
      timestamp TEXT,
      author TEXT,
      text TEXT,
      kind TEXT,
      amount INTEGER,
      amount_text TEXT,
      icon TEXT,
      parts_json TEXT,
      colors_json TEXT,
      is_member INTEGER,
      is_moderator INTEGER,
      is_owner INTEGER
    )`
  );

  ensureIconColumn();
  ensureColorsColumn();
  ensureBadgeColumns();
  ensureModerationColumns();
  ensureIndexes();
  insertStmt = db.prepare(
    `INSERT OR IGNORE INTO comments
     (id, video_id, timestamp_ms, timestamp, author, text, kind, amount, amount_text, icon, parts_json, colors_json, is_member, is_moderator, is_owner, author_channel_id)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`
  );

  // Comments saved before channel IDs were recorded are linked by name when that viewer returns.
  linkAuthorStmt = db.prepare(
    "UPDATE comments SET author_channel_id = ? WHERE author = ? AND author_channel_id IS NULL"
  );

  return dbPath;
}

function saveComment(msg) {
  if (!insertStmt || !msg || !msg.id) return;

  const partsJson = JSON.stringify(msg.parts || []);
  const colorsJson = JSON.stringify(msg.colors || null);
  try {
    const result = insertStmt.run(
      msg.id,
      msg.video_id || null,
      msg.timestamp_ms ?? null,
      msg.timestamp || null,
      msg.author || null,
      msg.text || null,
      msg.kind || null,
      msg.amount ?? null,
      msg.amount_text || null,
      msg.icon || null,
      partsJson,
      colorsJson,
      msg.is_member ? 1 : 0,
      msg.is_moderator ? 1 : 0,
      msg.is_owner ? 1 : 0,
      msg.author_channel_id || null
    );
    if (result.changes > 0 && msg.author_channel_id && msg.author) {
      linkAuthorStmt.run(msg.author_channel_id, msg.author);
    }
    if (result.changes > 0) commentEvents.emit('comment', msg);
    return result.changes > 0;
  } catch (err) {
    console.warn("saveComment sqlite error:", err.message || err);
  }
}

function getRecentComments(limit = DEFAULT_LIMIT, filter = {}) {
  return new Promise((resolve) => {
    if (!db) {
      resolve([]);
      return;
    }
    try {
      const lim = Number.isFinite(limit) ? Math.max(1, Math.min(limit, 500)) : DEFAULT_LIMIT;
      const params=[];
      let where='WHERE deleted_ms IS NULL';
      if(filter.supportOnly){
        const videoId=filter.videoId||db.prepare('SELECT video_id FROM comments WHERE deleted_ms IS NULL ORDER BY timestamp_ms DESC, rowid DESC LIMIT 1').get()?.video_id||'';
        where+=" AND kind IN ('paid','sticker','membership','gift_purchase','gift_redeem') AND COALESCE(video_id,'') = ?";
        params.push(videoId);
      }
      const unlimited=filter.supportOnly&&limit===null;
      if(!unlimited)params.push(lim);
      const sql = `SELECT id, video_id, timestamp_ms, timestamp, author, text, kind, amount, amount_text, icon, parts_json, colors_json, is_member, is_moderator, is_owner, author_channel_id
                   FROM comments
                   ${where}
                   ORDER BY timestamp_ms DESC, rowid DESC
                   ${unlimited?'':'LIMIT ?'}`;
      const rows = db.prepare(sql).all(...params);
      const result = rows.reverse().map((r) => {
        let parts = [];
        let colors = null;
        try {
          parts = r.parts_json ? JSON.parse(r.parts_json) : [];
        } catch (_) {
          parts = [];
        }
        try {
          colors = r.colors_json ? JSON.parse(r.colors_json) : null;
        } catch (_) {
          colors = null;
        }
        return {
          id: r.id,
          video_id: r.video_id,
          timestamp_ms: r.timestamp_ms,
          timestamp: r.timestamp,
          author: r.author,
          text: r.text,
          kind: r.kind,
          amount: r.amount,
          amount_text: r.amount_text,
          icon: r.icon,
          colors,
          parts,
          is_member: r.is_member === 1,
          is_moderator: r.is_moderator === 1,
          is_owner: r.is_owner === 1,
          author_channel_id: r.author_channel_id,
        };
      });
      resolve(result);
    } catch (err) {
      console.warn("getRecentComments sqlite error:", err.message || err);
      resolve([]);
    }
  });
}

/** Hides a comment removed on YouTube. The row stays so a re-delivered copy cannot reappear. */
function markCommentDeleted(id, deletedMs = Date.now()) {
  if (!db || !id) return 0;
  try {
    return db.prepare("UPDATE comments SET deleted_ms = ? WHERE id = ? AND deleted_ms IS NULL").run(deletedMs, id).changes;
  } catch (err) {
    console.warn("markCommentDeleted sqlite error:", err.message || err);
    return 0;
  }
}

function markAuthorCommentsDeleted(channelId, videoId, deletedMs = Date.now()) {
  if (!db || !channelId) return 0;
  try {
    return db
      .prepare("UPDATE comments SET deleted_ms = ? WHERE author_channel_id = ? AND video_id = ? AND deleted_ms IS NULL")
      .run(deletedMs, channelId, videoId).changes;
  } catch (err) {
    console.warn("markAuthorCommentsDeleted sqlite error:", err.message || err);
    return 0;
  }
}

function getDeletedCommentIds(limit = DELETED_ID_LIMIT) {
  if (!db) return [];
  try {
    return db
      .prepare("SELECT id FROM comments WHERE deleted_ms IS NOT NULL ORDER BY deleted_ms DESC LIMIT ?")
      .all(limit)
      .map((row) => row.id);
  } catch (err) {
    console.warn("getDeletedCommentIds sqlite error:", err.message || err);
    return [];
  }
}

function getSupportComments(videoId) {
  return getRecentComments(null,{supportOnly:true,videoId});
}

/** A viewer is their channel ID, or their name on comments that have none. */
function viewerKey(row) {
  return row?.author_channel_id || row?.author || "";
}

function uniqueViewers(viewers) {
  return Array.isArray(viewers)
    ? Array.from(new Set(viewers.map((v) => (v || "").trim()).filter(Boolean)))
    : [];
}

function getViewerFirstSeenMap(viewers) {
  if (!db) return new Map();
  const list = uniqueViewers(viewers);
  if (list.length === 0) return new Map();
  const placeholders = list.map(() => "?").join(",");
  try {
    const rows = db
      .prepare(
        `SELECT COALESCE(author_channel_id, author) AS viewer, MIN(timestamp_ms) AS first_ts
         FROM comments
         WHERE COALESCE(author_channel_id, author) IN (${placeholders})
         GROUP BY COALESCE(author_channel_id, author)`
      )
      .all(...list);
    return new Map(rows.map((r) => [r.viewer, Number(r.first_ts) || 0]));
  } catch (err) {
    console.warn("getViewerFirstSeenMap sqlite error:", err.message || err);
    return new Map();
  }
}

function getViewerVideoFirstSeenRows(viewers) {
  if (!db) return [];
  const list = uniqueViewers(viewers);
  if (list.length === 0) return [];
  const placeholders = list.map(() => "?").join(",");
  try {
    return db
      .prepare(
        `SELECT COALESCE(author_channel_id, author) AS viewer, video_id, MIN(timestamp_ms) AS first_ts
         FROM comments
         WHERE COALESCE(author_channel_id, author) IN (${placeholders})
           AND video_id IS NOT NULL AND video_id != ''
         GROUP BY COALESCE(author_channel_id, author), video_id`
      )
      .all(...list);
  } catch (err) {
    console.warn("getViewerVideoFirstSeenRows sqlite error:", err.message || err);
    return [];
  }
}

function getVideoFirstSeenMap() {
  if (!db) return new Map();
  try {
    const rows = db
      .prepare(
        `SELECT video_id, MIN(timestamp_ms) AS first_ts
         FROM comments
         WHERE video_id IS NOT NULL AND video_id != ''
         GROUP BY video_id`
      )
      .all();
    const map = new Map();
    for (const r of rows) {
      if (!r || !r.video_id) continue;
      map.set(r.video_id, Number(r.first_ts) || 0);
    }
    return map;
  } catch (err) {
    console.warn("getVideoFirstSeenMap sqlite error:", err.message || err);
    return new Map();
  }
}

function closeChatStore() {
  if (db) {
    try {
      db.close();
    } catch (_) {}
    db = null;
  }
  insertStmt = null;
  linkAuthorStmt = null;
}

module.exports = {
  onCommentSaved,
  initChatStore,
  saveComment,
  getRecentComments,
  getSupportComments,
  viewerKey,
  getViewerFirstSeenMap,
  getVideoFirstSeenMap,
  getViewerVideoFirstSeenRows,
  markCommentDeleted,
  markAuthorCommentsDeleted,
  getDeletedCommentIds,
  closeChatStore,
  getDbPath: () => dbPath,
};
