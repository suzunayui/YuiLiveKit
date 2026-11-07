// Decides whether a chat comment joins the participant queue.
const PARTICIPANT_KEYWORD = "参加希望";

// Comment ids per queue. Ids decide what was already seen, because a comment's
// request key changes once its author gets linked to a channel ID.
const seenIdsByBucket = new WeakMap();

function seenIdsOf(bucket) {
  if (!seenIdsByBucket.has(bucket)) {
    seenIdsByBucket.set(bucket, new Set(Array.from(bucket.values(), (item) => String(item.id))));
  }
  return seenIdsByBucket.get(bucket);
}

function authorNameOf(msg) {
  return typeof msg?.author === "string" ? msg.author.trim() : "";
}

function authorIdOf(msg) {
  return msg?.author_channel_id || authorNameOf(msg) || `anonymous:${msg?.id || msg?.timestamp_ms || Date.now()}`;
}

/**
 * Comments are re-read on every poll, so each one must leave a record the first
 * time it is seen. A repeat request from someone already waiting is stored as
 * "deleted"; otherwise it would join the queue once their first request is handled.
 */
function acceptParticipantComment(bucket, msg) {
  if (!msg || msg.kind !== "text") return false;
  const text = typeof msg.text === "string" ? msg.text : "";
  if (!text.includes(PARTICIPANT_KEYWORD)) return false;

  const authorId = authorIdOf(msg);
  const id = String(msg.id || `${msg.timestamp_ms || 0}_${msg.author || ""}_${text}`);
  const requestKey = `${authorId}::${id}`;
  const seenIds = seenIdsOf(bucket);
  if (bucket.has(requestKey) || seenIds.has(id)) return false;

  // Requests saved before channel IDs were recorded use the author's name.
  const authorName = authorNameOf(msg);
  let alreadyWaiting = false;
  for (const item of bucket.values()) {
    const itemAuthorId = item.author_id || item.author_key;
    if ((itemAuthorId === authorId || itemAuthorId === authorName) && item.status === "waiting") {
      alreadyWaiting = true;
      break;
    }
  }

  seenIds.add(id);
  const acceptedMs = Number(msg.timestamp_ms) || Date.now();
  bucket.set(requestKey, {
    id,
    video_id: msg.video_id || "default",
    author_key: requestKey,
    author_id: authorId,
    author: msg.author || "(no name)",
    icon: msg.icon || "",
    text,
    accepted_ms: acceptedMs,
    queue_order: acceptedMs,
    status: alreadyWaiting ? "deleted" : "waiting",
  });
  return true;
}

module.exports = { PARTICIPANT_KEYWORD, acceptParticipantComment };
