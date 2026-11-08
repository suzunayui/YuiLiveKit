// Settings window. Chat text is only ever set with textContent.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MAX_ITEMS = 300;
const STATUS_LABELS = { starting: '接続中…', running: '取得中', reconnecting: '再接続中…', stopped: '停止中' };
const PLATFORM_LABELS = { youtube: 'YouTube', twitch: 'Twitch' };
const SUPPORT_KINDS = ['paid', 'sticker', 'membership', 'gift_purchase', 'gift_redeem', 'bits'];
const $ = id => document.getElementById(id);

const items = new Map();
const statuses = {};
let support = [];
let participants = [];
let appearance = null;
let parts = [];
let viewers = { names: {}, blocked: [] };
const translationsByKey = new Map();
let media = null;
let speakers = [];

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

function button(text, onClick) {
  const node = element('button', '', text);
  node.addEventListener('click', event => { event.stopPropagation(); onClick(); });
  return node;
}

function showError(id, message) {
  $(id).textContent = message || '';
  $(id).hidden = !message;
}

async function attempt(resultId, action, success) {
  try {
    const value = await action();
    if (resultId) $(resultId).textContent = typeof success === 'function' ? success(value) : (success || '');
    return value;
  } catch (error) {
    if (resultId) $(resultId).textContent = String(error);
    return undefined;
  }
}

const formatTime = ms => new Date(ms).toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' });
const formatDate = ms => new Date(ms).toLocaleDateString('ja-JP', { month: 'numeric', day: 'numeric' });

function messageText(message) {
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
  return text;
}

// --- Tabs ------------------------------------------------------------------------------

const tabButtons = () => [...$('tab-buttons').querySelectorAll('button[data-tab]')];

function readList(key) {
  try {
    const list = JSON.parse(recall(key) || '[]');
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

function selectTab(name) {
  // A hidden tab, or one remembered from an older version, falls back to the first shown tab.
  const target = tabButtons().find(tab => tab.dataset.tab === name && !tab.hidden) || tabButtons().find(tab => !tab.hidden);
  name = target.dataset.tab;
  for (const tab of tabButtons()) tab.setAttribute('aria-selected', String(tab.dataset.tab === name));
  for (const panel of document.querySelectorAll('[data-panel]')) panel.hidden = panel.dataset.panel !== name;
  remember('tab', name);
  if (name === 'obs') refreshPlugin();
}
for (const tab of tabButtons()) tab.addEventListener('click', () => selectTab(tab.dataset.tab));

// Tabs can be dragged into the order the user wants, and unused ones hidden. Both are kept in
// this window's storage. Tabs that are new since the order was saved keep their place at the end.
function applyTabOrder() {
  const order = readList('tab-order');
  const tabs = tabButtons();
  const rank = tab => (order.includes(tab.dataset.tab) ? order.indexOf(tab.dataset.tab) : order.length + tabs.indexOf(tab));
  $('tab-buttons').append(...tabs.sort((a, b) => rank(a) - rank(b)));
}

function tabLabel(tab) {
  return tab.firstChild.textContent.trim();
}

function renderTabVisibility() {
  const hidden = readList('tab-hidden');
  for (const tab of tabButtons()) tab.hidden = hidden.includes(tab.dataset.tab);
  const shown = tabButtons().filter(tab => !tab.hidden).length;
  $('tab-visibility').replaceChildren(...tabButtons().map(tab => {
    const box = element('input');
    box.type = 'checkbox';
    box.dataset.show = tab.dataset.tab;
    box.checked = !tab.hidden;
    // The last shown tab cannot be hidden, so the window never ends up empty.
    box.disabled = !tab.hidden && shown === 1;
    box.addEventListener('change', () => {
      const next = readList('tab-hidden').filter(name => name !== tab.dataset.tab);
      if (!box.checked) next.push(tab.dataset.tab);
      remember('tab-hidden', JSON.stringify(next));
      renderTabVisibility();
      if (tab.getAttribute('aria-selected') === 'true' && !box.checked) selectTab('');
    });
    const label = element('label', 'inline');
    label.append(box, document.createTextNode(tabLabel(tab)));
    return label;
  }));
}
applyTabOrder();
renderTabVisibility();

let draggedTab = null;
for (const tab of tabButtons()) tab.draggable = true;
$('tab-buttons').addEventListener('dragstart', event => {
  draggedTab = event.target.closest('button');
  if (!draggedTab) return;
  draggedTab.classList.add('dragging');
  event.dataTransfer.effectAllowed = 'move';
  event.dataTransfer.setData('text/plain', draggedTab.dataset.tab);
});
$('tab-buttons').addEventListener('dragover', event => {
  if (!draggedTab) return;
  event.preventDefault();
  const over = event.target.closest('button');
  if (!over || over === draggedTab) return;
  const after = event.clientY > over.getBoundingClientRect().top + over.offsetHeight / 2;
  over.parentNode.insertBefore(draggedTab, after ? over.nextSibling : over);
});
$('tab-buttons').addEventListener('drop', event => event.preventDefault());
$('tab-buttons').addEventListener('dragend', () => {
  if (!draggedTab) return;
  draggedTab.classList.remove('dragging');
  draggedTab = null;
  remember('tab-order', JSON.stringify(tabButtons().map(tab => tab.dataset.tab)));
  renderTabVisibility();
});

// --- Comments --------------------------------------------------------------------------

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
  item.append(header, messageText(message));
  item.append(viewerActions(message));
  const translated = element('span', 'text translation', translationsByKey.get(message.key) || '');
  translated.hidden = !translationsByKey.has(message.key);
  item.append(translated);
  return item;
}

/** Same as speech::viewer_key in the app. */
function viewerKey(message) {
  return message.author_id ? `${message.platform}:${message.author_id}` : `${message.platform}:name:${message.author}`;
}

function viewerActions(message) {
  const key = viewerKey(message);
  const actions = element('span', 'actions');
  actions.append(
    button('読み方', async () => {
      const reading = prompt(`${message.author} さんの名前の読み方（空にすると元に戻します）`, viewers.names[key] || '');
      if (reading === null) return;
      viewers = await invoke('set_viewer_name', { viewer: key, reading });
      renderViewers();
    }),
    button(viewers.blocked.includes(key) ? 'ブロック解除' : 'ブロック', async () => {
      viewers = await invoke('set_blocked', { viewer: key, blocked: !viewers.blocked.includes(key) });
      renderViewers();
      for (const node of document.querySelectorAll('#comments li')) refreshViewerButton(node);
    }),
  );
  actions.dataset.viewer = key;
  return actions;
}

function refreshViewerButton(item) {
  const actions = item.querySelector('.actions[data-viewer]');
  if (!actions) return;
  actions.lastElementChild.textContent = viewers.blocked.includes(actions.dataset.viewer) ? 'ブロック解除' : 'ブロック';
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
  support = support.filter(s => !keys.includes(s.key));
  renderSupport();
  $('comment-count').textContent = `（${items.size}）`;
}

function clearMessages() {
  $('comments').replaceChildren();
  items.clear();
  support = [];
  renderSupport();
  $('comment-count').textContent = '';
}

// --- Status and collection -------------------------------------------------------------

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

$('youtube').value = recall('youtube');
$('twitch').value = recall('twitch');

// Enter in either box starts too.
$('connect').addEventListener('submit', async event => {
  event.preventDefault();
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

// --- Greetings -------------------------------------------------------------------------

function renderOhayo(ohayo) {
  $('ohayo-count').textContent = String(ohayo.count);
  $('ohayo-detected').textContent = String(ohayo.detected);
  $('ohayo-adjustment').textContent = (ohayo.adjustment > 0 ? '+' : '') + ohayo.adjustment;
}
$('ohayo-plus').addEventListener('click', async () => renderOhayo(await invoke('adjust_ohayo', { delta: 1 })));
$('ohayo-minus').addEventListener('click', async () => renderOhayo(await invoke('adjust_ohayo', { delta: -1 })));
$('ohayo-reset').addEventListener('click', async () => renderOhayo(await invoke('reset_ohayo')));

function keywordLines(text) {
  return text.split(/\r?\n|,|、/).map(s => s.trim()).filter(Boolean);
}

async function saveToolSettings(resultId) {
  const saved = await attempt(resultId, () => invoke('set_tool_settings', {
    participantKeyword: $('participant-keyword').value,
    ohayoKeywords: keywordLines($('ohayo-keywords').value),
  }), '保存しました');
  if (saved) renderToolSettings(saved);
}
function renderToolSettings(settings) {
  $('participant-keyword').value = settings.participant_keyword;
  $('ohayo-keywords').value = settings.ohayo_keywords.join('\n');
}
$('ohayo-keywords-save').addEventListener('click', () => saveToolSettings('ohayo-keywords-result'));
$('participant-keyword-save').addEventListener('click', () => saveToolSettings('participant-keyword-result'));

// --- Support ---------------------------------------------------------------------------

function renderSupport() {
  const list = $('support');
  list.replaceChildren();
  const unreadOnly = $('support-unread-only').checked;
  for (const entry of support) {
    if (unreadOnly && entry.read) continue;
    const item = renderItem(entry);
    item.classList.toggle('unread', !entry.read);
    item.title = entry.read ? 'クリックで未読に戻す' : 'クリックで既読にする';
    item.addEventListener('click', async () => {
      support = await invoke('set_support_read', { ids: [entry.row_id], read: !entry.read });
      renderSupport();
    });
    list.append(item);
  }
  const unread = support.filter(s => !s.read).length;
  $('support-unread').textContent = unread ? `（${unread}）` : '';
}
$('support-unread-only').addEventListener('change', renderSupport);
$('support-read-all').addEventListener('click', async () => {
  support = await invoke('set_support_read', { ids: support.filter(s => !s.read).map(s => s.row_id), read: true });
  renderSupport();
});
$('support-copy').addEventListener('click', () => attempt('support-result', async () => {
  const lines = support.filter(s => !s.read).map(s => [s.author, s.amount_text, s.text].filter(Boolean).join(' '));
  await navigator.clipboard.writeText(lines.join('\n'));
  return lines.length;
}, count => `${count}件をコピーしました`));

// --- Participants ----------------------------------------------------------------------

function renderParticipants(next) {
  participants = next;
  const list = $('participants');
  list.replaceChildren();
  const hideDone = $('participants-hide-done').checked;
  let rank = 0;
  for (const entry of participants) {
    if (hideDone && entry.status === 'done') continue;
    const waiting = entry.status === 'waiting';
    const item = element('li', waiting ? '' : 'done');
    item.dataset.id = entry.id;
    item.draggable = waiting;
    item.append(element('span', 'time', waiting ? `${++rank}` : '完了'));
    item.append(element('span', 'name', entry.author));
    const actions = element('span', 'actions');
    actions.append(
      button(waiting ? '完了' : '戻す', async () => renderParticipants(await attempt(null, () =>
        invoke('set_participant_status', { id: entry.id, status: waiting ? 'done' : 'waiting' })) || participants)),
      button('削除', async () => renderParticipants(await invoke('set_participant_status', { id: entry.id, status: 'removed' }))),
    );
    item.append(actions, element('span', 'text', entry.text));
    list.append(item);
  }
}

let dragged = null;
$('participants').addEventListener('dragstart', event => {
  dragged = event.target.closest('li');
  dragged?.classList.add('dragging');
});
$('participants').addEventListener('dragover', event => {
  event.preventDefault();
  const over = event.target.closest('li');
  if (!dragged || !over || over === dragged || !over.draggable) return;
  const after = event.clientY > over.getBoundingClientRect().top + over.offsetHeight / 2;
  over.parentNode.insertBefore(dragged, after ? over.nextSibling : over);
});
$('participants').addEventListener('dragend', async () => {
  dragged?.classList.remove('dragging');
  dragged = null;
  const ids = [...$('participants').querySelectorAll('li[draggable="true"]')].map(li => Number(li.dataset.id));
  renderParticipants(await invoke('reorder_participants', { ids }));
});
$('participants-hide-done').addEventListener('change', () => renderParticipants(participants));
$('participants-clear-done').addEventListener('click', async () => renderParticipants(await invoke('clear_participants', { mode: 'done' })));
$('participants-clear-all').addEventListener('click', async () => {
  if (confirm('参加管理の一覧を全て消しますか？')) renderParticipants(await invoke('clear_participants', { mode: 'all' }));
});

// --- Appearance ------------------------------------------------------------------------

const LOOK_FIELDS = {
  comments: [['font_size', '文字の大きさ', 'number'], ['max', '表示する件数', 'number'], ['background', '背景色（#RRGGBBAA）', 'text'],
    ['text_color', '文字の色', 'text'], ['author_color', '名前の色', 'text'], ['show_icons', 'アイコンを表示', 'checkbox']],
  niconico: [['font_size', '文字の大きさ', 'number'], ['duration', '流れる秒数', 'number'], ['color', '文字の色', 'text']],
  participants: [['font_size', '文字の大きさ', 'number'], ['horizontal', '横に並べる', 'checkbox'], ['title', '見出し', 'text']],
  ohayo: [['font_size', '文字の大きさ', 'number'], ['label', '表示する言葉', 'text']],
  subtitles: [['font_size', '日本語の文字の大きさ', 'number'], ['english_size', '英語の文字の大きさ', 'number'], ['duration', '表示する秒数', 'number'],
    ['alignment', '寄せ（left / center / right）', 'text'], ['text_color', '日本語の色', 'text'], ['english_color', '英語の色', 'text']],
};

function renderLook() {
  const form = $('look-form');
  form.replaceChildren();
  for (const part of parts) {
    const values = appearance[part.id];
    const fields = LOOK_FIELDS[part.id];
    if (!values || !fields) continue;
    const group = element('fieldset', 'look-part');
    group.dataset.part = part.id;
    group.append(element('legend', '', part.label));
    for (const [field, label, type] of fields) {
      const input = element('input');
      input.type = type;
      input.dataset.field = field;
      if (type === 'checkbox') input.checked = Boolean(values[field]);
      else input.value = values[field];
      input.addEventListener('change', () => {
        values[field] = type === 'checkbox' ? input.checked : type === 'number' ? Number(input.value) : input.value;
      });
      const wrapper = element('label', type === 'checkbox' ? 'inline' : '');
      if (type === 'checkbox') wrapper.append(input, document.createTextNode(label));
      else wrapper.append(document.createTextNode(label), input);
      group.append(wrapper);
    }
    form.append(group);
  }
}

$('look-save').addEventListener('click', () => attempt('look-result', async () => {
  appearance = await invoke('save_appearance', { appearance });
  renderLook();
}, '保存しました。OBS の表示にも反映しました。'));

// --- OBS -------------------------------------------------------------------------------

const PLUGIN_STATES = { installed: '導入済み（最新）', outdated: '古い版が入っています', missing: '未導入' };
async function refreshPlugin() {
  const status = await invoke('obs_plugin_status');
  $('plugin-state').textContent = PLUGIN_STATES[status.state] + (status.obs_running ? '（OBS 起動中）' : '');
  $('plugin-path').textContent = status.path;
  $('plugin-install').disabled = status.state === 'installed';
}
$('plugin-install').addEventListener('click', () => attempt('plugin-result', async () => {
  $('plugin-result').textContent = '導入中…';
  await invoke('install_obs_plugin');
  await refreshPlugin();
}, '導入しました。OBS を起動すると、今のシーンに YuiLiveKit のグループが追加されます。'));
function renderPartUrls() {
  const body = $('part-urls').tBodies[0];
  body.replaceChildren();
  for (const part of parts) {
    const row = element('tr');
    const drag = element('a', 'drag', '⠿ ドラッグ');
    drag.href = part.drag_url;
    drag.draggable = true;
    drag.title = `${part.label} を OBS にドラッグして追加`;
    // Only for dragging; following the link would take this window away.
    drag.addEventListener('click', event => event.preventDefault());
    drag.addEventListener('dragstart', event => {
      event.dataTransfer.effectAllowed = 'copyLink';
      event.dataTransfer.setData('text/uri-list', part.drag_url);
      event.dataTransfer.setData('text/plain', part.drag_url);
    });
    const copy = element('button', '', 'コピー');
    copy.addEventListener('click', () => attempt('copy-result', () => navigator.clipboard.writeText(part.url), `${part.label} の URL をコピーしました`));
    const url = element('td');
    url.append(element('code', '', part.url));
    const action = element('td');
    action.className = 'part-actions';
    action.append(drag, copy);
    row.append(element('td', '', part.label), url, element('td', '', `${part.width}×${part.height}`), action);
    body.append(row);
  }
}

// --- Legacy import ---------------------------------------------------------------------

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

// --- Reading aloud ---------------------------------------------------------------------

function fillSpeakers(select, selectedId) {
  select.replaceChildren(...speakers.map(s => { const o = element('option', '', s.name); o.value = s.id; return o; }));
  if (!speakers.some(s => s.id === selectedId)) {
    const o = element('option', '', `話者 ${selectedId}（VOICEVOX 未接続）`);
    o.value = selectedId;
    select.append(o);
  }
  select.value = String(selectedId);
}

async function loadSpeakers(settings, ai) {
  try {
    speakers = await invoke('voicevox_speakers');
    $('speech-result').textContent = `VOICEVOX に接続しました（話者 ${speakers.length}）`;
  } catch (error) {
    speakers = [];
    $('speech-result').textContent = String(error);
  }
  fillSpeakers($('speech-speaker'), settings.voice.speaker);
  fillSpeakers($('ai-speaker'), ai.voice.speaker);
}

const voiceOf = prefix => ({
  speaker: Number($(`${prefix}-speaker`).value),
  speed: Number($(`${prefix}-speed`).value),
  volume: Number($(`${prefix}-volume`).value),
});

function renderSpeech(settings) {
  $('speech-enabled').checked = settings.enabled;
  $('speech-speed').value = settings.voice.speed;
  $('speech-volume').value = settings.voice.volume;
  $('speech-max-queue').value = settings.max_queue;
  $('speech-read-name').checked = settings.read_name;
  $('speech-replacements').value = settings.replacements.map(r => `${r.from}=>${r.to}`).join('\n');
  $('speech-ng').value = settings.ng_words.join('\n');
}

function readReplacements(text) {
  return text.split(/\r?\n/).map(line => line.split(/=>|＝>/)).filter(p => p.length >= 2 && p[0].trim())
    .map(([from, ...to]) => ({ from: from.trim(), to: to.join('=>').trim() }));
}

$('speech-save').addEventListener('click', () => attempt('speech-result', async () => {
  renderSpeech(await invoke('set_speech_settings', { settings: {
    enabled: $('speech-enabled').checked, voice: voiceOf('speech'), read_name: $('speech-read-name').checked,
    replacements: readReplacements($('speech-replacements').value), ng_words: keywordLines($('speech-ng').value),
    max_queue: Number($('speech-max-queue').value),
  } }));
}, '保存しました'));
$('speech-test').addEventListener('click', () => invoke('speak_test', { text: $('speech-test-text').value, voice: voiceOf('speech') }));
$('speech-reload-speakers').addEventListener('click', async () => {
  const state = await invoke('get_voice_state');
  await loadSpeakers(state.speech, state.ai);
});

function renderViewers() {
  const list = $('viewer-list');
  list.replaceChildren();
  const keys = new Set([...Object.keys(viewers.names), ...viewers.blocked]);
  for (const key of keys) {
    const item = element('li');
    const label = [viewers.names[key] ? `読み方「${viewers.names[key]}」` : '', viewers.blocked.includes(key) ? 'ブロック中' : '']
      .filter(Boolean).join('、');
    item.append(element('span', '', `${key}: ${label}`));
    const actions = element('span', 'actions');
    if (viewers.names[key]) actions.append(button('読み方を消す', async () => { viewers = await invoke('set_viewer_name', { viewer: key, reading: '' }); renderViewers(); }));
    if (viewers.blocked.includes(key)) actions.append(button('ブロック解除', async () => { viewers = await invoke('set_blocked', { viewer: key, blocked: false }); renderViewers(); }));
    item.append(actions);
    list.append(item);
  }
}

// --- AI replies ------------------------------------------------------------------------

function renderAi(settings) {
  $('ai-enabled').checked = settings.enabled;
  $('ai-model').value = settings.model;
  $('ai-speed').value = settings.voice.speed;
  $('ai-volume').value = settings.voice.volume;
  $('ai-interval').value = settings.interval_seconds;
  $('ai-hourly').value = settings.hourly_limit;
  $('ai-max-chars').value = settings.max_comment_chars;
  $('ai-triggers').value = settings.triggers.join('\n');
  $('ai-persona').value = settings.persona;
}

function renderKeyState(hasKey) {
  $('ai-key-state').textContent = hasKey ? 'キーは保存されています' : 'キーは未設定です';
}

$('ai-save').addEventListener('click', () => attempt('ai-result', async () => {
  renderAi(await invoke('set_ai_settings', { settings: {
    enabled: $('ai-enabled').checked, model: $('ai-model').value, persona: $('ai-persona').value,
    triggers: keywordLines($('ai-triggers').value), voice: voiceOf('ai'), interval_seconds: Number($('ai-interval').value),
    hourly_limit: Number($('ai-hourly').value), max_comment_chars: Number($('ai-max-chars').value),
  } }));
}, '保存しました'));
$('ai-key-save').addEventListener('click', async () => {
  renderKeyState(await invoke('set_api_key', { key: $('ai-key').value }));
  $('ai-key').value = '';
});
$('ai-key-delete').addEventListener('click', async () => renderKeyState(await invoke('set_api_key', { key: null })));

// --- Emoji -----------------------------------------------------------------------------

function renderEmoji(list) {
  const box = $('emoji-list');
  box.replaceChildren();
  for (const emoji of list) {
    const item = element('li');
    if (emoji.url) {
      const img = element('img');
      img.src = emoji.url;
      img.alt = emoji.code;
      item.append(img);
    } else item.append(element('span'));
    item.append(element('span', 'name', emoji.code));
    const reading = element('input');
    reading.value = emoji.reading || '';
    reading.placeholder = '読み方';
    reading.addEventListener('change', async () => renderEmoji(await invoke('set_emoji', { code: emoji.code, reading: reading.value, volume: null, clearSound: false })));
    item.append(reading);
    const actions = element('span', 'actions');
    actions.append(button(emoji.sound ? '効果音を変える' : '効果音を選ぶ', () => attempt('emoji-result', async () =>
      renderEmoji(await invoke('choose_emoji_sound', { code: emoji.code })))));
    if (emoji.sound) {
      const volume = element('input');
      volume.type = 'number'; volume.min = 0; volume.max = 2; volume.step = 0.1; volume.value = emoji.volume;
      volume.title = '音量';
      volume.addEventListener('change', async () => renderEmoji(await invoke('set_emoji', { code: emoji.code, reading: null, volume: Number(volume.value), clearSound: false })));
      actions.append(volume, button('効果音を外す', async () => renderEmoji(await invoke('set_emoji', { code: emoji.code, reading: null, volume: null, clearSound: true }))));
    }
    item.append(actions);
    box.append(item);
  }
  if (!list.length) box.append(element('li', '', 'まだ絵文字は出ていません。'));
}

// --- Subtitles and translation ---------------------------------------------------------

function options(select, list, value) {
  select.replaceChildren(...list.map(([v, label]) => { const o = element('option', '', label); o.value = v; return o; }));
  if (list.some(([v]) => String(v) === String(value))) select.value = String(value);
}

function renderRuntime(runtime) {
  $('runtime-status').textContent = runtime.status;
  const label = (done, text) => (done ? `${text}（導入済み）` : text);
  $('install-speech').textContent = label(runtime.speech, '音声字幕を導入（約100MB）');
  $('install-cuda').textContent = label(runtime.cuda, 'GPU 高速化を導入（約1.3GB）');
  $('install-translation').textContent = label(runtime.translation, 'コメント翻訳を導入');
  $('install-speech').disabled = runtime.busy || runtime.speech;
  $('install-cuda').disabled = runtime.busy || runtime.cuda;
  $('install-translation').disabled = runtime.busy || runtime.translation;
}

function renderSubtitleState(state) {
  $('sub-status').textContent = state.status;
  $('sub-level').value = state.level;
  $('sub-start').disabled = !state.ready || state.running;
  $('sub-stop').disabled = !state.running;
  $('sub-reload').disabled = state.running;
  const settings = media?.subtitle_settings;
  options($('sub-microphone'), state.microphones.map(d => [d.id, d.name]), settings?.microphone ?? '');
  options($('sub-device'), [['cpu', 'CPU（低速）'], ...state.gpus.map(g => [`cuda:${g.id}`, g.name])], settings?.device ?? 'cpu');
}

function renderTranslationState(state) {
  $('tr-status').textContent = `${state.status}（訳 ${state.translated}、スキップ ${state.skipped}、待ち ${state.queued}）`;
}

function readSubtitleSettings() {
  const microphone = $('sub-microphone').value;
  return {
    microphone: microphone === '' ? null : Number(microphone), device: $('sub-device').value || 'cpu',
    model: $('sub-model').value, threshold: Number($('sub-threshold').value),
    start_with_chat: $('sub-with-chat').checked, mask_profanity: $('sub-mask').checked,
  };
}

async function loadMedia() {
  try {
    media = await invoke('get_media_state');
  } catch (error) {
    $('runtime-status').textContent = String(error);
    return;
  }
  if (!media) return;
  const s = media.subtitle_settings;
  $('sub-model').value = s.model;
  $('sub-threshold').value = s.threshold;
  $('sub-with-chat').checked = s.start_with_chat;
  $('sub-mask').checked = s.mask_profanity;
  renderSubtitleState(media.subtitles);
  $('tr-enabled').checked = media.translation_settings.enabled;
  options($('tr-device'), [['cpu', 'CPU（低速）'], ...media.gpus.map(g => [`cuda:${g.id}`, g.name])], media.translation_settings.device);
  renderTranslationState(media.translation);
  renderRuntime(media.runtime);
}

$('sub-save').addEventListener('click', () => attempt('sub-status', async () => {
  media.subtitle_settings = await invoke('set_subtitle_settings', { settings: readSubtitleSettings() });
}, '保存しました'));
$('sub-start').addEventListener('click', () => attempt('sub-status', async () => {
  media.subtitle_settings = await invoke('set_subtitle_settings', { settings: readSubtitleSettings() });
  await invoke('start_subtitles');
}, '字幕を開始しています…'));
$('sub-stop').addEventListener('click', () => invoke('stop_subtitles'));
$('sub-reload').addEventListener('click', () => invoke('reload_devices'));
$('sub-clear').addEventListener('click', () => invoke('clear_subtitle'));
$('tr-save').addEventListener('click', () => attempt('tr-status', async () => {
  media.translation_settings = await invoke('set_translation_settings', { settings: { enabled: $('tr-enabled').checked, device: $('tr-device').value } });
}, '保存しました'));
for (const feature of ['speech', 'cuda', 'translation']) {
  $(`install-${feature}`).addEventListener('click', () => attempt('runtime-status', () => invoke('install_feature', { feature }), '導入が完了しました'));
}

function showTranslation(key, ja) {
  translationsByKey.set(key, ja);
  const line = items.get(key)?.querySelector('.translation');
  if (line) { line.textContent = ja; line.hidden = false; }
}

// --- Events and start ------------------------------------------------------------------

await listen('chat', ({ payload }) => {
  if (payload.type === 'message') {
    addMessage(payload);
    if (SUPPORT_KINDS.includes(payload.kind)) { support.push({ ...payload, read: false }); renderSupport(); }
  } else if (payload.type === 'deleted') removeMessages(payload.keys);
  else if (payload.type === 'reset') clearMessages();
  else if (payload.type === 'status') {
    statuses[payload.platform] = { status: payload.status, detail: payload.detail };
    renderStatuses();
  } else if (payload.type === 'participants') renderParticipants(payload.items);
  else if (payload.type === 'ohayo') renderOhayo(payload);
  else if (payload.type === 'appearance') { appearance = payload; delete appearance.type; renderLook(); }
  else if (payload.type === 'speech') $('speech-result').textContent = payload.message;
  else if (payload.type === 'subtitle') $('sub-latest').textContent = [payload.ja, payload.en].filter(Boolean).join(' / ');
  else if (payload.type === 'subtitle_state') { if (media) media.subtitles = payload; renderSubtitleState(payload); }
  else if (payload.type === 'translation_state') renderTranslationState(payload);
  else if (payload.type === 'translation') showTranslation(payload.key, payload.ja);
  else if (payload.type === 'translations_cleared') {
    translationsByKey.clear();
    for (const line of document.querySelectorAll('#comments .translation')) { line.textContent = ''; line.hidden = true; }
  } else if (payload.type === 'runtime') renderRuntime(payload);
});
await listen('server-error', ({ payload }) => showError('server-error', payload));

const state = await invoke('get_state');
parts = state.parts;
renderPartUrls();
showError('server-error', state.server_error);
Object.assign(statuses, state.statuses);
renderStatuses();
state.messages.forEach(addMessage);
support = state.support;
renderSupport();
renderParticipants(state.participants);
renderOhayo(state.ohayo);
renderToolSettings(state.tool_settings);
appearance = state.appearance;
renderLook();
if (!state.legacy_imported && state.legacy_candidates.length) {
  for (const path of state.legacy_candidates) $('legacy-path').append(element('option', '', path));
  $('legacy').hidden = false;
}
const voice = await invoke('get_voice_state');
viewers = voice.viewers;
renderSpeech(voice.speech);
renderAi(voice.ai);
renderKeyState(voice.has_api_key);
renderViewers();
renderEmoji(voice.emoji);
for (const node of document.querySelectorAll('#comments li')) refreshViewerButton(node);
loadSpeakers(voice.speech, voice.ai);
await loadMedia();
selectTab(recall('tab') || 'comments');
