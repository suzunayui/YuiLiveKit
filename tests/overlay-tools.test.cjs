// The overlay's niconico, participant, greeting, subtitle and appearance parts, against the overlay_demo example.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const path = require('node:path');
const { chromium } = require('playwright');

const exe = path.join(__dirname, '../src-tauri/target/debug/examples/overlay_demo.exe');
const PORT = 15031;

function startDemo() {
  const child = spawn(exe, [String(PORT)], { stdio: ['pipe', 'pipe', 'inherit'] });
  let buffer = '';
  const waiting = [];
  child.stdout.on('data', chunk => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf('\n')) >= 0) {
      buffer = buffer.slice(index + 1);
      waiting.shift()?.();
    }
  });
  const next = () => new Promise(resolve => waiting.push(resolve));
  return {
    ready: next(),
    send(command) { const done = next(); child.stdin.write(JSON.stringify(command) + '\n'); return done; },
    stop: () => new Promise(resolve => { child.once('exit', resolve); child.kill(); }),
  };
}

function appearance(changes) {
  const base = {
    comments: { font_size: 24, max: 12, background: '#000000c7', text_color: '#ffffff', author_color: '#ffd8f8', show_icons: true },
    niconico: { font_size: 56, duration: 30, color: '#ffffff' },
    participants: { font_size: 24, horizontal: false, title: '参加待ち' },
    ohayo: { font_size: 40, label: 'おはよう' },
    subtitles: { font_size: 46, english_size: 32, duration: 2, alignment: 'center', text_color: '#ffffff', english_color: '#ffffff' },
  };
  for (const [part, values] of Object.entries(changes)) Object.assign(base[part], values);
  return base;
}

/** Opens one part the way OBS does: its own page at the source's size. */
async function openPart(browser, part, errors, viewport = { width: 800, height: 400 }) {
  const page = await browser.newPage({ viewport });
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${PORT}/overlay/${part}`);
  return page;
}

const visibleParts = page => page.$$eval('.widget', nodes => nodes.filter(n => !n.hidden).map(n => n.id));

test('each part is its own page: niconico, participants and greetings', async () => {
  const demo = startDemo();
  await demo.ready;
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    await demo.send({ op: 'save_appearance', appearance: appearance({}) });
    await demo.send({ op: 'start', stream: 'v1' });
    const errors = [];
    const nico = await openPart(browser, 'niconico', errors, { width: 1920, height: 880 });
    const comments = await openPart(browser, 'comments', errors);
    const participants = await openPart(browser, 'participants', errors);
    const ohayo = await openPart(browser, 'ohayo', errors);
    assert.deepEqual(await visibleParts(nico), ['niconico'], 'a page shows only its own part');
    assert.deepEqual(await visibleParts(ohayo), ['ohayo']);
    await ohayo.waitForFunction(() => document.getElementById('ohayo-label').textContent === 'おはよう');

    await demo.send({ op: 'message', id: 'n1', author: 'A', author_id: 'UC1', text: 'flying <b>text</b>' });
    await nico.waitForFunction(() => [...document.querySelectorAll('#niconico .bullet')].some(b => b.textContent === 'flying <b>text</b>'));
    await comments.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 1);
    assert.equal(await comments.$$eval('.bullet', n => n.length), 0, 'only the niconico page flies comments');
    assert.equal(await nico.$$eval('.msg', n => n.length), 0, 'only the comments page lists them');
    await demo.send({ op: 'remove', id: 'n1' });
    await nico.waitForFunction(() => !document.querySelector('#niconico .bullet'), null, { timeout: 5000 });

    await demo.send({ op: 'message', id: 'p1', author: 'Player', author_id: 'UC2', text: '参加希望です' });
    await demo.send({ op: 'message', id: 'p2', author: 'Player', author_id: 'UC2', text: '参加希望！' });
    await demo.send({ op: 'message', id: 'p3', author: 'Other', author_id: 'UC3', text: '参加希望' });
    await participants.waitForFunction(() => document.querySelectorAll('#participants-list li').length === 2);
    assert.deepEqual(await participants.$$eval('#participants-list li', items => items.map(i => i.textContent)), ['1Player', '2Other'], 'a repeat request does not queue twice (K3)');

    await demo.send({ op: 'message', id: 'o1', author: 'A', author_id: 'UC1', text: 'おはよう！' });
    await demo.send({ op: 'message', id: 'o2', author: 'A', author_id: 'UC1', text: 'おはよ〜' });
    await demo.send({ op: 'message', id: 'o3', author: 'B', author_id: 'UC4', text: 'おはなしして' });
    await ohayo.waitForFunction(() => document.getElementById('ohayo-count').textContent === '1');

    await demo.send({ op: 'save_appearance', appearance: appearance({ comments: { font_size: 40 }, ohayo: { label: 'Good morning' } }) });
    await ohayo.waitForFunction(() => document.getElementById('ohayo-label').textContent === 'Good morning');
    await comments.waitForFunction(() => getComputedStyle(document.querySelector('#comments .msg')).fontSize === '40px');
    const box = await comments.$eval('#comments', n => [n.offsetWidth, n.offsetHeight]);
    assert.deepEqual(box, [800, 400], 'the part fills its source');
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await demo.stop();
  }
});

test('subtitles and comment translations on the overlay', async () => {
  const demo = startDemo();
  await demo.ready;
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    await demo.send({ op: 'save_appearance', appearance: appearance({}) });
    await demo.send({ op: 'start', stream: 'v1' });
    const errors = [];
    const page = await openPart(browser, 'subtitles', errors);
    const comments = await openPart(browser, 'comments', errors);
    await page.waitForFunction(() => document.getElementById('sub-box').hidden);

    await demo.send({ op: 'subtitle', ja: '<b>こんにちは</b>', en: 'Hello' });
    await page.waitForFunction(() => !document.getElementById('sub-box').hidden);
    assert.equal(await page.textContent('#sub-ja'), '<b>こんにちは</b>', 'subtitles are text');
    assert.equal(await page.textContent('#sub-en'), 'Hello');
    await page.reload();
    await page.waitForFunction(() => document.getElementById('sub-ja').textContent === '<b>こんにちは</b>');
    await page.waitForFunction(() => document.getElementById('sub-box').hidden, null, { timeout: 4000 });

    await demo.send({ op: 'message', id: 't1', author: 'Guest', author_id: 'UC9', text: 'Hello everyone' });
    await comments.waitForFunction(() => document.querySelector('[data-key="youtube:t1"]'));
    await demo.send({ op: 'translate', key: 'youtube:t1', ja: '<i>みなさん</i>こんにちは' });
    await comments.waitForFunction(() => document.querySelector('[data-key="youtube:t1"] .translation').textContent === '<i>みなさん</i>こんにちは');
    assert.equal(await comments.$$eval('#comments i', n => n.length), 0);
    // Redrawing translations after a reload comes from the app's translation results (Rust tests cover it).
    await demo.send({ op: 'clear_translations' });
    await comments.waitForFunction(() => document.querySelector('[data-key="youtube:t1"] .translation').hidden);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await demo.stop();
  }
});
