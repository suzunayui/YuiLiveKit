// The overlay's niconico, participant, greeting and layout parts, against the overlay_demo example.
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

function layout(changes) {
  const base = {
    comments: { visible: true, x: 24, y: 420, width: 720, height: 636, font_size: 24, max: 12, background: '#000000c7', text_color: '#ffffff', author_color: '#ffd8f8', show_icons: true },
    niconico: { visible: true, x: 0, y: 100, width: 1920, height: 880, font_size: 56, duration: 30, color: '#ffffff' },
    participants: { visible: true, x: 1380, y: 40, width: 500, height: 600, font_size: 24, horizontal: false, title: '参加待ち' },
    ohayo: { visible: true, x: 1500, y: 920, width: 380, height: 120, font_size: 40, label: 'おはよう' },
  };
  for (const [part, values] of Object.entries(changes)) Object.assign(base[part], values);
  return base;
}

test('niconico, participants, greetings and named layouts', async () => {
  const demo = startDemo();
  await demo.ready;
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    await demo.send({ op: 'save_layout', name: 'default', layout: layout({}) });
    await demo.send({ op: 'save_layout', name: 'game', layout: layout({ comments: { x: 900, visible: false }, ohayo: { label: 'Good morning' } }) });
    await demo.send({ op: 'start', stream: 'v1' });
    const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
    const game = await browser.newPage({ viewport: { width: 1280, height: 720 } });
    const errors = [];
    for (const p of [page, game]) p.on('pageerror', error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${PORT}/overlay`);
    await game.goto(`http://127.0.0.1:${PORT}/overlay?layout=game`);
    await page.waitForFunction(() => !document.getElementById('niconico').hidden);
    await game.waitForFunction(() => document.getElementById('comments').hidden);
    assert.equal(await game.textContent('#ohayo-label'), 'Good morning', 'each scene gets its own layout');
    assert.equal(await page.textContent('#ohayo-label'), 'おはよう');

    await demo.send({ op: 'message', id: 'n1', author: 'A', author_id: 'UC1', text: 'flying <b>text</b>' });
    await page.waitForFunction(() => [...document.querySelectorAll('#niconico .bullet')].some(b => b.textContent === 'flying <b>text</b>'));
    await demo.send({ op: 'remove', id: 'n1' });
    await page.waitForFunction(() => !document.querySelector('#niconico .bullet'), null, { timeout: 5000 });

    await demo.send({ op: 'message', id: 'p1', author: 'Player', author_id: 'UC2', text: '参加希望です' });
    await demo.send({ op: 'message', id: 'p2', author: 'Player', author_id: 'UC2', text: '参加希望！' });
    await demo.send({ op: 'message', id: 'p3', author: 'Other', author_id: 'UC3', text: '参加希望' });
    await page.waitForFunction(() => document.querySelectorAll('#participants-list li').length === 2);
    assert.deepEqual(await page.$$eval('#participants-list li', items => items.map(i => i.textContent)), ['1Player', '2Other'], 'a repeat request does not queue twice (K3)');

    await demo.send({ op: 'message', id: 'o1', author: 'A', author_id: 'UC1', text: 'おはよう！' });
    await demo.send({ op: 'message', id: 'o2', author: 'A', author_id: 'UC1', text: 'おはよ〜' });
    await demo.send({ op: 'message', id: 'o3', author: 'B', author_id: 'UC4', text: 'おはなしして' });
    await page.waitForFunction(() => document.getElementById('ohayo-count').textContent === '1');
    await game.waitForFunction(() => document.getElementById('ohayo-count').textContent === '1');

    await demo.send({ op: 'save_layout', name: 'default', layout: layout({ comments: { x: 500, font_size: 40 }, ohayo: { visible: false } }) });
    await page.waitForFunction(() => document.getElementById('comments').style.left === '500px' && document.getElementById('ohayo').hidden);
    assert.equal(await game.$eval('#comments', n => n.hidden), true, 'saving one layout leaves other scenes alone');
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await demo.stop();
  }
});
