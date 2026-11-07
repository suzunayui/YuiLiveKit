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

// window.__TAURI__ stand-in: records invoke calls and lets the test fire events.
const fakeTauri = state => {
  const listeners = {};
  window.calls = [];
  window.fire = (name, payload) => (listeners[name] || []).forEach(fn => fn({ payload }));
  window.__TAURI__ = {
    core: {
      invoke: async (command, args) => {
        window.calls.push([command, args]);
        if (command === 'get_state') return state;
        if (command === 'start_chat' && !args.youtube && !args.twitch) throw 'YouTubeかTwitchのどちらかを入力してください';
        if (command === 'import_legacy') return { streams: 3, viewers: 10, messages: 42 };
        return null;
      },
    },
    event: { listen: async (name, fn) => { (listeners[name] ||= []).push(fn); } },
  };
};

const message = (key, extra = {}) => ({
  type: 'message', key, stream_id: 1, platform: 'youtube', id: key, author_id: 'UC1', author: 'Viewer', icon: null,
  kind: 'text', text: 'hi', parts: [{ type: 'text', text: 'hi' }], amount_micros: null, currency: null, amount_text: null,
  colors: null, badges: {}, created_ms: Date.UTC(2026, 10, 8, 3, 4), first_time: false, last_stream_ms: null, last_stream_ago: null, ...extra,
});

test('the settings window lists chat safely and drives the app', async () => {
  const server = await serve();
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(fakeTauri, {
      overlay_url: 'http://127.0.0.1:5000/overlay', server_error: null, statuses: {},
      messages: [message('youtube:old', { text: 'earlier' })],
      legacy_imported: false, legacy_candidates: ['C:\\Users\\a\\AppData\\Roaming\\yt-overlay\\comments.db'],
    });
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.waitForFunction(() => document.querySelectorAll('#comments li').length === 1);
    assert.equal(await page.textContent('#overlay-url'), 'http://127.0.0.1:5000/overlay');
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

    await page.evaluate(m => window.fire('chat', m), message('youtube:new', {
      author: '<b>name</b>', parts: [{ type: 'text', text: '<img src=x onerror=alert(1)>' }], first_time: true,
    }));
    await page.evaluate(m => window.fire('chat', m), message('youtube:back', { last_stream_ms: Date.UTC(2026, 10, 1), last_stream_ago: 3 }));
    await page.waitForFunction(() => document.querySelectorAll('#comments li').length === 3);
    assert.equal(await page.$$eval('#comments img', imgs => imgs.length), 0, 'markup stays text (K7)');
    assert.match(await page.textContent('[data-key="youtube:new"]'), /<b>name<\/b>初見<img src=x onerror=alert\(1\)>/);
    assert.match(await page.textContent('[data-key="youtube:back"]'), /前回 11\/1（3回前）/);

    await page.evaluate(() => window.fire('chat', { type: 'deleted', keys: ['youtube:new'] }));
    assert.equal(await page.$('[data-key="youtube:new"]'), null, 'deleted comments leave the list (K1)');
    await page.evaluate(() => window.fire('chat', { type: 'reset' }));
    assert.equal(await page.$$eval('#comments li', items => items.length), 0);

    await page.click('#legacy-import');
    await page.waitForFunction(() => document.querySelector('#legacy-result').textContent.includes('42件'));
    await page.evaluate(() => window.fire('chat', { type: 'status', platform: 'youtube', status: 'stopped', detail: '配信が終了したため、チャットの取得を停止しました' }));
    assert.match(await page.textContent('#statuses'), /停止中（配信が終了/);
    assert.equal(await page.isDisabled('#start'), false);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    server.close();
  }
});
