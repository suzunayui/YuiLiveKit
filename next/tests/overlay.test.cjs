// Opens /overlay in Microsoft Edge (the same Chromium family as OBS's browser source) against
// the overlay_demo example, which takes scripted chat on stdin. Run: node --test tests/
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const path = require('node:path');
const { chromium } = require('playwright');

const exe = path.join(__dirname, '../src-tauri/target/debug/examples/overlay_demo.exe');
const PORT = 15030;
const URL = `http://127.0.0.1:${PORT}/overlay`;

function startDemo() {
  const child = spawn(exe, [String(PORT)], { stdio: ['pipe', 'pipe', 'inherit'] });
  const lines = [];
  let waiting = null;
  let buffer = '';
  child.stdout.on('data', chunk => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, index).trim();
      buffer = buffer.slice(index + 1);
      lines.push(line);
      if (waiting) { const resolve = waiting; waiting = null; resolve(line); }
    }
  });
  const next = () => lines.length ? Promise.resolve(lines.shift()) : new Promise(resolve => { waiting = resolve; lines.length = 0; });
  return {
    ready: next(),
    async send(command) {
      lines.length = 0;
      const answered = new Promise(resolve => { waiting = resolve; });
      child.stdin.write(JSON.stringify(command) + '\n');
      await answered;
    },
    stop: () => new Promise(resolve => { child.once('exit', resolve); child.kill(); }),
  };
}

const keys = page => page.$$eval('#comments .msg', nodes => nodes.map(n => n.dataset.key));

test('the overlay follows chat, deletions, new collections and app restarts', async () => {
  let demo = startDemo();
  await demo.ready;
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await demo.send({ op: 'start', stream: 'v1' });
    await demo.send({ op: 'message', id: 'before', author: 'Early', author_id: 'UC0', text: 'sent before OBS opened' });
    await page.goto(URL);
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 1);

    await demo.send({ op: 'message', id: 'xss', author: '<b>name</b>', author_id: 'UC1', text: '<img src=x onerror=alert(1)>' });
    await demo.send({ op: 'message', id: 'evil-icon', author: 'Icon', author_id: 'UC2', text: 'hi', icon: 'https://evil.example/a.png' });
    await demo.send({ op: 'message', id: 'paid', author: 'Fan', author_id: 'UC3', text: 'thanks', kind: 'paid', amount_text: '¥10,000' });
    await demo.send({ op: 'message', id: 'dollar', author: 'Fan2', author_id: 'UC4', text: 'thanks', kind: 'paid', amount_text: '$100.00' });
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 5);
    assert.deepEqual(await keys(page), ['youtube:before', 'youtube:xss', 'youtube:evil-icon', 'youtube:paid', 'youtube:dollar']);
    assert.equal(await page.$$eval('#comments img', imgs => imgs.length), 0, 'markup stays text and unknown hosts are dropped (K7)');
    assert.equal(await page.textContent('[data-key="youtube:xss"]'), '<b>name</b><img src=x onerror=alert(1)>');
    assert.ok(await page.$eval('[data-key="youtube:paid"]', n => n.classList.contains('tier4')), '¥10,000 is the top yen tier');
    assert.ok(await page.$eval('[data-key="youtube:dollar"]', n => n.classList.contains('tier1')), '$100 is not counted as yen (K8)');

    await demo.send({ op: 'remove', id: 'xss' });
    await demo.send({ op: 'remove_author', author_id: 'UC3' });
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 3);
    assert.deepEqual(await keys(page), ['youtube:before', 'youtube:evil-icon', 'youtube:dollar'], 'deleted comments disappear (K1)');

    await demo.send({ op: 'message', id: 'xss', author: '<b>name</b>', author_id: 'UC1', text: 'again' });
    await page.reload();
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 3);
    assert.ok(!(await keys(page)).includes('youtube:xss'), 'a re-delivered deleted comment stays hidden (K2)');

    await demo.send({ op: 'start', stream: 'v2' });
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 0);
    await demo.send({ op: 'message', id: 'now', author: 'Now', author_id: 'UC5', text: 'new stream' });
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 1);

    await demo.stop();
    await page.waitForFunction(() => document.querySelectorAll('#comments .msg').length === 0);
    demo = startDemo();
    await demo.ready;
    await demo.send({ op: 'start', stream: 'v3' });
    await demo.send({ op: 'message', id: 'back', author: 'Back', author_id: 'UC6', text: 'after restart' });
    await page.waitForFunction(() => document.querySelector('#comments .msg')?.dataset.key === 'youtube:back', null, { timeout: 15000 });

    for (let i = 0; i < 20; i++) await demo.send({ op: 'message', id: `many${i}`, author: 'Many', author_id: 'UC7', text: String(i) });
    await page.waitForFunction(() => document.querySelector('#comments .msg:last-child')?.dataset.key === 'youtube:many19');
    assert.equal((await keys(page)).length, 12, 'only the newest comments stay on screen');
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
    await demo.stop();
  }
});
