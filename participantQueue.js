// Decides whether a chat comment joins the participant queue.
const PARTICIPANT_KEYWORD = "参加希望";

function authorIdOf(msg) {
  const author = typeof msg?.author === "string" ? msg.author.trim() : "";
  return author || `anonymous:${msg?.id || msg?.timestamp_ms || Date.now()}`;
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
  if (bucket.has(requestKey)) return false;

  let alreadyWaiting = false;
  for (const item of bucket.values()) {
    if ((item.author_id || item.author_key) === authorId && item.status === "waiting") {
      alreadyWaiting = true;
      break;
    }
  }

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
