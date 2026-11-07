// Settings window. Chat text is only ever set with textContent.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MAX_ITEMS = 300;
const STATUS_LABELS = { starting: '接続中…', running: '取得中', reconnecting: '再接続中…', stopped: '停止中' };
const PLATFORM_LABELS = { youtube: 'YouTube', twitch: 'Twitch' };
const $ = id => document.getElementById(id);
const items = new Map();
const statuses = {};

function remember(key, value) {
  try { localStorage.setItem(key, value); } catch { /* storage may be unavailable */ }
}
function recall(key) {
  try { return localStorage.getItem(key) || ''; } catch { return ''; }
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function formatTime(ms) {
  return new Date(ms).toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' });
}

function formatDate(ms) {
  return new Date(ms).toLocaleDateString('ja-JP', { month: 'numeric', day: 'numeric' });
}

function renderItem(message) {
  const item = element('li');
  item.dataset.key = message.key;
  item.append(element('span', 'time', formatTime(message.created_ms)));
  const header = element('span');
  header.append(element('span', 'name', message.author));
  if (message.first_time) header.append(element('span', 'badge first', '初見'));
  else if (message.last_stream_ms) {
    header.append(element('span', 'badge last', `前回 ${formatDate(message.last_stream_ms)}（${message.last_stream_ago}回前）`));
  }
  if (message.amount_text) header.append(element('span', 'badge last', message.amount_text));
  item.append(header);
  const text = element('span', 'text');
  for (const part of message.parts || []) {
    if (part.type === 'text') text.append(document.createTextNode(part.text));
    else {
      const img = element('img');
      img.src = part.url;
      img.alt = part.alt || '';
      text.append(img);
    }
  }
  if (!(message.parts || []).length) text.textContent = message.text;
  item.append(text);
  return item;
}

function addMessage(message) {
  if (items.has(message.key)) return;
  const list = $('comments');
  const stick = list.scrollTop + list.clientHeight >= list.scrollHeight - 24;
  const item = renderItem(message);
  list.append(item);
  items.set(message.key, item);
  while (list.children.length > MAX_ITEMS) {
    items.delete(list.firstElementChild.dataset.key);
    list.firstElementChild.remove();
  }
  if (stick) list.scrollTop = list.scrollHeight;
  $('comment-count').textContent = `（${items.size}）`;
}

function removeMessages(keys) {
  for (const key of keys) {
    items.get(key)?.remove();
    items.delete(key);
  }
  $('comment-count').textContent = `（${items.size}）`;
}

function clearMessages() {
  $('comments').replaceChildren();
  items.clear();
  $('comment-count').textContent = '';
}

function renderStatuses() {
  const box = $('statuses');
  box.replaceChildren();
  for (const [platform, { status, detail }] of Object.entries(statuses)) {
    const text = `${PLATFORM_LABELS[platform] || platform}: ${STATUS_LABELS[status] || status}${detail ? `（${detail}）` : ''}`;
    box.append(element('span', detail ? 'error' : '', text));
  }
  const running = Object.values(statuses).some(s => s.status !== 'stopped');
  $('start').disabled = running;
  $('stop').disabled = !running;
}

function showError(id, message) {
  $(id).textContent = message || '';
  $(id).hidden = !message;
}

$('youtube').value = recall('youtube');
$('twitch').value = recall('twitch');

$('start').addEventListener('click', async () => {
  showError('start-error', '');
  remember('youtube', $('youtube').value.trim());
  remember('twitch', $('twitch').value.trim());
  try {
    await invoke('start_chat', { youtube: $('youtube').value, twitch: $('twitch').value });
  } catch (error) {
    showError('start-error', String(error));
  }
});

$('stop').addEventListener('click', () => invoke('stop_chat'));

$('legacy-import').addEventListener('click', async () => {
  $('legacy-import').disabled = true;
  $('legacy-result').textContent = '取り込み中…';
  try {
    const summary = await invoke('import_legacy', { path: $('legacy-path').value });
    $('legacy-result').textContent = `${summary.messages}件のコメントを取り込みました。`;
  } catch (error) {
    $('legacy-result').textContent = String(error);
    $('legacy-import').disabled = false;
  }
});

await listen('chat', ({ payload }) => {
  if (payload.type === 'message') addMessage(payload);
  else if (payload.type === 'deleted') removeMessages(payload.keys);
  else if (payload.type === 'reset') clearMessages();
  else if (payload.type === 'status') {
    statuses[payload.platform] = { status: payload.status, detail: payload.detail };
    renderStatuses();
  }
});
await listen('server-error', ({ payload }) => showError('server-error', payload));

const state = await invoke('get_state');
$('overlay-url').textContent = state.overlay_url;
showError('server-error', state.server_error);
Object.assign(statuses, state.statuses);
renderStatuses();
state.messages.forEach(addMessage);
if (!state.legacy_imported && state.legacy_candidates.length) {
  for (const path of state.legacy_candidates) $('legacy-path').append(element('option', '', path));
  $('legacy').hidden = false;
}
