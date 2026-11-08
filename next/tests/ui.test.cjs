// Runs the settings window's page in Edge with a stand-in for Tauri's invoke and events.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const root = path.join(__dirname, '../ui');
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' };

function serve() {
  const server = http.createServer((req, res) => {
    const file = path.join(root, decodeURIComponent(new URL(req.url, 'http://x').pathname));
    if (!file.startsWith(root) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) { res.writeHead(404).end(); return; }
    res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' }).end(fs.readFileSync(file));
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server)));
}

const area = (x, extra = {}) => ({ visible: true, x, y: 100, width: 400, height: 300, font_size: 24, ...extra });
const defaultLayout = {
  comments: { ...area(24), max: 12, background: '#000000c7', text_color: '#ffffff', author_color: '#ffd8f8', show_icons: true },
  niconico: { ...area(0, { visible: false }), duration: 8, color: '#ffffff' },
  participants: { ...area(1380, { visible: false }), horizontal: false, title: '参加待ち' },
  ohayo: { ...area(1500, { visible: false }), label: 'おはよう' },
};

const message = (key, extra = {}) => ({
  type: 'message', key, row_id: Number(key.replace(/\D/g, '')) || 1, stream_id: 1, platform: 'youtube', id: key, author_id: 'UC1',
  author: 'Viewer', icon: null, kind: 'text', text: 'hi', parts: [{ type: 'text', text: 'hi' }], amount_micros: null, currency: null,
  amount_text: null, colors: null, badges: {}, created_ms: Date.UTC(2026, 10, 8, 3, 4), first_time: false, last_stream_ms: null,
  last_stream_ago: null, ...extra,
});

// window.__TAURI__ stand-in: records invoke calls, answers like the app, and lets the test fire events.
const fakeTauri = ({ state }) => {
  const listeners = {};
  window.calls = [];
  window.fire = (name, payload) => (listeners[name] || []).forEach(fn => fn({ payload }));
  let support = state.support;
  let participants = state.participants;
  let layouts = state.layouts;
  let ohayo = state.ohayo;
  let voice = state.voice;
  window.__TAURI__ = {
    core: {
      invoke: async (command, args = {}) => {
        window.calls.push([command, args]);
        switch (command) {
          case 'get_state': return state;
          case 'start_chat':
            if (!args.youtube && !args.twitch) throw 'YouTubeかTwitchのどちらかを入力してください';
            return null;
          case 'import_legacy': return { streams: 3, viewers: 10, messages: 42 };
          case 'set_support_read':
            support = support.map(s => (args.ids.includes(s.row_id) ? { ...s, read: args.read } : s));
            return support;
          case 'set_participant_status':
            participants = participants.map(p => (p.id === args.id ? { ...p, status: args.status } : p)).filter(p => p.status !== 'removed');
            return participants;
          case 'clear_participants': participants = participants.filter(p => args.mode === 'done' && p.status !== 'done'); return participants;
          case 'reorder_participants': return participants;
          case 'adjust_ohayo': ohayo = { ...ohayo, adjustment: ohayo.adjustment + args.delta, count: Math.max(0, ohayo.count + args.delta) }; return ohayo;
          case 'reset_ohayo': ohayo = { detected: 0, adjustment: 0, count: 0 }; return ohayo;
          case 'set_tool_settings': return { participant_keyword: args.participantKeyword, ohayo_keywords: args.ohayoKeywords };
          case 'save_layout':
            if (!/^[A-Za-z0-9_-]{1,32}$/.test(args.name)) throw '配置の名前は半角英数字、-、_ の32文字までにしてください';
            layouts = { ...layouts, [args.name]: args.layout };
            return layouts;
          case 'delete_layout': { const { [args.name]: _, ...rest } = layouts; layouts = rest; return layouts; }
          case 'obs_plugin_status': return { state: 'missing', path: 'C:\\ProgramData\\obs-studio\\plugins\\yui-live-kit\\bin\\64bit\\yui-live-kit.dll', obs_running: false };
          case 'install_obs_plugin': return { state: 'installed', path: '', obs_running: false };
          case 'get_voice_state': return voice;
          case 'voicevox_speakers': return [{ id: 2, name: '四国めたん（ノーマル）' }, { id: 3, name: 'ずんだもん（ノーマル）' }];
          case 'set_speech_settings': voice = { ...voice, speech: args.settings }; return args.settings;
          case 'set_ai_settings': voice = { ...voice, ai: args.settings }; return args.settings;
          case 'set_api_key': voice = { ...voice, has_api_key: Boolean(args.key) }; return voice.has_api_key;
          case 'set_viewer_name': {
            const names = { ...voice.viewers.names };
            if (args.reading) names[args.viewer] = args.reading; else delete names[args.viewer];
            voice = { ...voice, viewers: { ...voice.viewers, names } };
            return voice.viewers;
          }
          case 'set_blocked': {
            const blocked = voice.viewers.blocked.filter(v => v !== args.viewer).concat(args.blocked ? [args.viewer] : []);
            voice = { ...voice, viewers: { ...voice.viewers, blocked } };
            return voice.viewers;
          }
          case 'set_emoji':
            voice = { ...voice, emoji: voice.emoji.map(e => (e.code === args.code ? { ...e, reading: args.reading ?? e.reading, sound: args.clearSound ? null : e.sound } : e)) };
            return voice.emoji;
          case 'choose_emoji_sound':
            voice = { ...voice, emoji: voice.emoji.map(e => (e.code === args.code ? { ...e, sound: 'C:/sounds/a.wav' } : e)) };
            return voice.emoji;
          default: return null;
        }
      },
    },
    event: { listen: async (name, fn) => { (listeners[name] ||= []).push(fn); } },
  };
};

async function open(browser, server, state) {
  const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(fakeTauri, { state });
  await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
  await page.waitForFunction(() => window.calls.some(c => c[0] === 'get_state'));
  return { page, errors };
}

function baseState(extra = {}) {
  return {
    overlay_url: 'http://127.0.0.1:5000/overlay', server_error: null, statuses: {},
    messages: [message('youtube:1', { text: 'earlier', parts: [{ type: 'text', text: 'earlier' }] })],
    participants: [
      { id: 1, status: 'waiting', author: 'First', icon: null, text: '参加希望', platform: 'youtube', created_ms: 1 },
      { id: 2, status: 'waiting', author: 'Second', icon: null, text: '参加希望！', platform: 'youtube', created_ms: 2 },
      { id: 3, status: 'done', author: 'Done', icon: null, text: '参加希望', platform: 'youtube', created_ms: 0 },
    ],
    ohayo: { detected: 2, adjustment: 0, count: 2 },
    support: [
      { ...message('youtube:11', { kind: 'paid', amount_text: '¥1,000', author: 'Fan', text: 'thanks' }), read: false },
      { ...message('youtube:12', { kind: 'membership', author: 'Member', text: 'joined' }), read: true },
    ],
    tool_settings: { participant_keyword: '参加希望', ohayo_keywords: ['おはよ', 'おっは'] },
    layouts: { default: defaultLayout },
    legacy_imported: false, legacy_candidates: ['C:\\Users\\a\\AppData\\Roaming\\YuiLiveKit\\comments.db'],
    voice: {
      speech: { enabled: false, voice: { speaker: 3, speed: 1, volume: 1 }, read_name: true, replacements: [], ng_words: [], max_queue: 20 },
      ai: { enabled: false, model: 'gpt-6-luna', persona: '', triggers: [], voice: { speaker: 2, speed: 1, volume: 1 }, interval_seconds: 20, hourly_limit: 30, max_comment_chars: 200 },
      viewers: { names: {}, blocked: [] }, has_api_key: false,
      emoji: [{ code: ':wave:', url: null, reading: null, sound: null, volume: 1 }],
    },
    ...extra,
  };
}

test('home and comments: collection, status, safe chat text and history', async () => {
  const server = await serve();
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    const { page, errors } = await open(browser, server, baseState());
    assert.equal(await page.isVisible('#legacy'), true, 'offers the one-time import');
    await page.click('#start');
    await page.waitForFunction(() => !document.querySelector('#start-error').hidden);
    assert.match(await page.textContent('#start-error'), /どちらか/);
    await page.fill('#youtube', '@yui');
    await page.click('#start');
    assert.deepEqual(await page.evaluate(() => window.calls.find(c => c[0] === 'start_chat' && c[1].youtube)), ['start_chat', { youtube: '@yui', twitch: '' }]);
    await page.evaluate(() => window.fire('chat', { type: 'status', platform: 'youtube', status: 'running', detail: null }));
    assert.equal(await page.textContent('#statuses'), 'YouTube: 取得中');
    assert.equal(await page.isDisabled('#start'), true);

    await page.click('[data-tab="comments"]');
    await page.evaluate(m => window.fire('chat', m), message('youtube:2', { author: '<b>name</b>', parts: [{ type: 'text', text: '<img src=x onerror=alert(1)>' }], first_time: true }));
    await page.evaluate(m => window.fire('chat', m), message('youtube:3', { last_stream_ms: Date.UTC(2026, 10, 1), last_stream_ago: 3 }));
    await page.waitForFunction(() => document.querySelectorAll('#comments li').length === 3);
    assert.equal(await page.$$eval('#comments img', imgs => imgs.length), 0, 'markup stays text (K7)');
    assert.match(await page.textContent('[data-key="youtube:2"]'), /<b>name<\/b>初見<img src=x onerror=alert\(1\)>/);
    assert.match(await page.textContent('[data-key="youtube:3"]'), /前回 11\/1（3回前）/);
    await page.evaluate(() => window.fire('chat', { type: 'deleted', keys: ['youtube:2'] }));
    assert.equal(await page.$('#comments [data-key="youtube:2"]'), null, 'deleted comments leave the list (K1)');

    await page.click('[data-tab="home"]');
    await page.click('#ohayo-plus');
    await page.waitForFunction(() => document.getElementById('ohayo-count').textContent === '3');
    await page.click('#ohayo-reset');
    await page.waitForFunction(() => document.getElementById('ohayo-count').textContent === '0');
    await page.fill('#ohayo-keywords', 'おはよ\nおっは\nohayo');
    await page.click('#ohayo-keywords-save');
    await page.waitForFunction(() => document.getElementById('ohayo-keywords-result').textContent === '保存しました');
    assert.deepEqual((await page.evaluate(() => window.calls.find(c => c[0] === 'set_tool_settings')))[1].ohayoKeywords, ['おはよ', 'おっは', 'ohayo']);
    await page.click('#legacy-import');
    await page.waitForFunction(() => document.querySelector('#legacy-result').textContent.includes('42件'));
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    server.close();
  }
});

test('support, participants, layout editor and OBS tabs', async () => {
  const server = await serve();
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    const { page, errors } = await open(browser, server, baseState());
    assert.equal(await page.textContent('#support-unread'), '（1）');
    await page.click('[data-tab="support"]');
    await page.check('#support-unread-only');
    assert.deepEqual(await page.$$eval('#support li', items => items.map(i => i.dataset.key)), ['youtube:11']);
    await page.click('#support [data-key="youtube:11"]');
    await page.waitForFunction(() => document.querySelectorAll('#support li').length === 0);
    assert.equal(await page.textContent('#support-unread'), '');
    await page.evaluate(m => window.fire('chat', m), message('youtube:13', { kind: 'paid', amount_text: '$5.00', author: 'New' }));
    await page.waitForFunction(() => document.querySelectorAll('#support li').length === 1);

    await page.click('[data-tab="participants"]');
    assert.deepEqual(await page.$$eval('#participants li .name', n => n.map(x => x.textContent)), ['First', 'Second', 'Done']);
    await page.click('#participants li:first-child .actions button:first-child');
    await page.waitForFunction(() => document.querySelector('#participants li:first-child').classList.contains('done'));
    await page.check('#participants-hide-done');
    assert.deepEqual(await page.$$eval('#participants li .name', n => n.map(x => x.textContent)), ['Second']);
    await page.evaluate(() => window.fire('chat', { type: 'participants', items: [] }));
    assert.equal(await page.$$eval('#participants li', n => n.length), 0, 'the list follows the app');

    await page.click('[data-tab="layout"]');
    assert.equal(await page.$$eval('#preview .box', b => b.length), 4);
    const box = await page.$('#preview .box.selected');
    const before = await box.boundingBox();
    await page.mouse.move(before.x + 20, before.y + 20);
    await page.mouse.down();
    await page.mouse.move(before.x + 120, before.y + 70, { steps: 4 });
    await page.mouse.up();
    const x = Number(await page.inputValue('#widget-form input[data-field="x"]'));
    assert.ok(x > 24, `dragging moves the box (x=${x})`);
    await page.fill('#widget-form input[data-field="font_size"]', '40');
    await page.dispatchEvent('#widget-form input[data-field="font_size"]', 'change');
    await page.click('#layout-save');
    await page.waitForFunction(() => document.getElementById('layout-result').textContent.startsWith('保存しました'));
    const saved = (await page.evaluate(() => window.calls.filter(c => c[0] === 'save_layout').pop()))[1];
    assert.equal(saved.name, 'default');
    assert.equal(saved.layout.comments.font_size, 40);
    assert.equal(saved.layout.comments.x, x);
    await page.fill('#layout-new-name', 'bad name');
    await page.click('#layout-add');
    await page.waitForFunction(() => document.getElementById('layout-result').textContent.includes('半角英数字'));
    await page.fill('#layout-new-name', 'game');
    await page.click('#layout-add');
    await page.waitForFunction(() => document.getElementById('layout-url').textContent.endsWith('?layout=game'));

    await page.click('[data-tab="obs"]');
    await page.waitForFunction(() => document.getElementById('plugin-state').textContent === '未導入');
    await page.click('#plugin-install');
    await page.waitForFunction(() => document.getElementById('plugin-result').textContent.startsWith('導入しました'));
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    server.close();
  }
});

test('reading aloud, AI replies, emoji and viewer settings', async () => {
  const server = await serve();
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    const { page, errors } = await open(browser, server, baseState());
    await page.click('[data-tab="speech"]');
    await page.waitForFunction(() => document.querySelectorAll('#speech-speaker option').length === 2);
    assert.equal(await page.inputValue('#speech-speaker'), '3');
    await page.check('#speech-enabled');
    await page.selectOption('#speech-speaker', '2');
    await page.fill('#speech-replacements', 'www=>わらわら\n草=>くさ\n壊れた行');
    await page.fill('#speech-ng', 'ネタバレ\n暴言');
    await page.click('#speech-save');
    await page.waitForFunction(() => document.getElementById('speech-result').textContent === '保存しました');
    const speech = (await page.evaluate(() => window.calls.find(c => c[0] === 'set_speech_settings')))[1].settings;
    assert.equal(speech.enabled, true);
    assert.equal(speech.voice.speaker, 2);
    assert.deepEqual(speech.replacements, [{ from: 'www', to: 'わらわら' }, { from: '草', to: 'くさ' }]);
    assert.deepEqual(speech.ng_words, ['ネタバレ', '暴言']);
    await page.click('#speech-test');
    assert.equal((await page.evaluate(() => window.calls.find(c => c[0] === 'speak_test')))[1].voice.speaker, 2);

    await page.click('[data-tab="comments"]');
    page.once('dialog', dialog => dialog.accept('ゆいちゃん'));
    await page.click('#comments li .actions button:first-child');
    await page.waitForFunction(() => window.calls.some(c => c[0] === 'set_viewer_name'));
    await page.click('#comments li .actions button:last-child');
    await page.waitForFunction(() => document.querySelector('#comments li .actions button:last-child').textContent === 'ブロック解除');
    await page.click('[data-tab="speech"]');
    assert.match(await page.textContent('#viewer-list'), /youtube:UC1: 読み方「ゆいちゃん」、ブロック中/);

    await page.click('[data-tab="ai"]');
    assert.equal(await page.textContent('#ai-key-state'), 'キーは未設定です');
    await page.fill('#ai-key', 'sk-test');
    await page.click('#ai-key-save');
    await page.waitForFunction(() => document.getElementById('ai-key-state').textContent === 'キーは保存されています');
    assert.equal(await page.inputValue('#ai-key'), '', 'the key does not stay on screen');
    await page.check('#ai-enabled');
    await page.fill('#ai-triggers', 'GPT\n教えて');
    await page.fill('#ai-persona', '関西弁で');
    await page.click('#ai-save');
    await page.waitForFunction(() => document.getElementById('ai-result').textContent === '保存しました');
    const ai = (await page.evaluate(() => window.calls.find(c => c[0] === 'set_ai_settings')))[1].settings;
    assert.deepEqual([ai.enabled, ai.triggers, ai.persona], [true, ['GPT', '教えて'], '関西弁で']);
    assert.ok(!JSON.stringify(ai).includes('sk-test'), 'the key is never part of the settings');

    await page.click('[data-tab="emoji"]');
    await page.fill('#emoji-list input', 'てをふる');
    await page.dispatchEvent('#emoji-list input', 'change');
    await page.waitForFunction(() => window.calls.some(c => c[0] === 'set_emoji' && c[1].reading === 'てをふる'));
    await page.click('#emoji-list .actions button');
    await page.waitForFunction(() => [...document.querySelectorAll('#emoji-list button')].some(b => b.textContent === '効果音を外す'));
    await page.evaluate(() => window.fire('chat', { type: 'speech', message: 'VOICEVOXに接続できません' }));
    assert.equal(await page.textContent('#speech-result'), 'VOICEVOXに接続できません');
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    server.close();
  }
});
