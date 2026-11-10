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

// What a viewer would see: parts that take up space on the page, not just ones without `hidden`.
const visibleParts = page => page.$$eval('.widget', nodes => nodes.filter(n => getComputedStyle(n).display !== 'none').map(n => n.id));

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
    assert.deepEqual(await visibleParts(comments), ['comments']);
    assert.deepEqual(await visibleParts(participants), ['participants']);
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
    for (const [name, page] of [['niconico', nico], ['comments', comments], ['participants', participants]]) {
      assert.equal(await page.$eval('#ohayo', n => n.getBoundingClientRect().width), 0, `the greeting count stays off the ${name} page`);
    }
    await demo.send({ op: 'subtitle', ja: 'こんにちは', en: 'Hello' });
    await ohayo.waitForFunction(() => document.getElementById('sub-ja').textContent === 'こんにちは');
    for (const [name, page] of [['niconico', nico], ['comments', comments], ['participants', participants], ['ohayo', ohayo]]) {
      assert.equal(await page.$eval('#sub-box', n => n.getBoundingClientRect().width), 0, `subtitles stay off the ${name} page`);
    }

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

test('flying comments keep to lanes, and the comment list never shows a cut-off comment', async () => {
  const demo = startDemo();
  await demo.ready;
  const browser = await chromium.launch({ channel: 'msedge' });
  try {
    // The default crossing time, so lanes free up as fast as they do for real.
    await demo.send({ op: 'save_appearance', appearance: appearance({ niconico: { duration: 8 } }) });
    await demo.send({ op: 'start', stream: 'v1' });
    const errors = [];
    const nico = await openPart(browser, 'niconico', errors, { width: 1920, height: 880 });
    // The list is shorter than twelve comments are tall, as it is at the plugin's default size.
    const comments = await openPart(browser, 'comments', errors, { width: 720, height: 400 });
    const texts = ['wwwww', 'かわいい', 'ないすー', 'これはニコニコ風に流れる少し長めのコメントです', '888888', 'こんにちは', '草', 'おつかれさま'];
    for (const [i, text] of texts.entries()) await demo.send({ op: 'message', id: `b${i}`, author: 'A', author_id: `UC${i}`, text });
    await nico.waitForFunction(count => document.querySelectorAll('#niconico .bullet').length === count, texts.length);
    await nico.waitForTimeout(300);
    const overlaps = await nico.$$eval('#niconico .bullet', bullets => {
      const boxes = bullets.map(b => ({ text: b.textContent, ...b.getBoundingClientRect().toJSON() }));
      const hit = (a, b) => a.left < b.right && b.left < a.right && a.top < b.bottom - 1 && b.top < a.bottom - 1;
      return boxes.flatMap((a, i) => boxes.slice(i + 1).filter(b => hit(a, b)).map(b => `${a.text} × ${b.text}`));
    });
    assert.deepEqual(overlaps, [], 'comments arriving together do not overlap');
    assert.ok(await nico.$$eval('#niconico .bullet', b => b.every(x => x.getBoundingClientRect().bottom <= innerHeight + 1)), 'every lane is inside the source');

    // More at once than there are lanes: the rest wait for a lane instead of overlapping, and
    // a deleted comment that is still waiting never flies.
    const overlapping = () => nico.$$eval('#niconico .bullet', bullets => {
      const boxes = bullets.map(b => b.getBoundingClientRect());
      const hit = (a, b) => a.left < b.right && b.left < a.right && a.top < b.bottom - 1 && b.top < a.bottom - 1;
      return boxes.some((a, i) => boxes.slice(i + 1).some(b => hit(a, b)));
    });
    await nico.evaluate(() => {
      window.flown = new Set();
      new MutationObserver(() => { for (const b of document.querySelectorAll('#niconico .bullet')) window.flown.add(b.textContent); })
        .observe(document.getElementById('niconico'), { childList: true });
    });
    for (let i = 0; i < 14; i++) await demo.send({ op: 'message', id: `flood${i}`, author: 'A', author_id: `UF${i}`, text: `まとめて${i}` });
    await demo.send({ op: 'remove', id: 'flood13' });
    assert.equal(await overlapping(), false, 'a flood does not overlap');
    assert.ok(await nico.$$eval('#niconico .bullet', b => b.length) < texts.length + 13, 'some wait their turn');
    await nico.waitForFunction(() => [...Array(13).keys()].every(i => window.flown.has(`まとめて${i}`)), null, { timeout: 15000 });
    assert.equal(await overlapping(), false, 'nor do the ones that waited');
    assert.equal(await nico.evaluate(() => window.flown.has('まとめて13')), false, 'the deleted one never flew');

    await comments.waitForFunction(last => document.querySelector('#comments .msg:last-child')?.dataset.key === last, 'youtube:flood12');
    const cut = async () => comments.$$eval('#comments .msg', rows => rows.filter(r => r.getBoundingClientRect().top < -0.5).map(r => r.dataset.key));
    assert.deepEqual(await cut(), [], 'no comment is cut off at the top');
    const shownBefore = await comments.$$eval('#comments .msg', rows => rows.length);
    assert.ok(shownBefore >= 3 && shownBefore < texts.length, `as many as fit are shown (${shownBefore})`);
    // A translation makes its comment taller; the list makes room again.
    await demo.send({ op: 'translate', key: 'youtube:flood12', ja: '日本語の訳が付くと、このコメントは一行ぶん高くなります' });
    await comments.waitForFunction(() => !document.querySelector('[data-key="youtube:flood12"] .translation').hidden);
    assert.deepEqual(await cut(), [], 'still none after a comment grows');
    // A bigger font from the settings does the same.
    await demo.send({ op: 'save_appearance', appearance: appearance({ comments: { font_size: 40 }, niconico: { duration: 8 } }) });
    await comments.waitForFunction(() => getComputedStyle(document.querySelector('#comments .msg')).fontSize === '40px');
    assert.deepEqual(await cut(), [], 'still none after the font grows');
    assert.ok(await comments.$$eval('#comments .msg', rows => rows.length) < shownBefore);

    // Starting again on the same stream puts the comments back on the list, but none of them
    // flies across the screen a second time.
    await nico.waitForFunction(() => document.querySelectorAll('#niconico .bullet').length === 0, null, { timeout: 15000 });
    await demo.send({ op: 'start', stream: 'v1' });
    await comments.waitForFunction(() => document.querySelector('#comments .msg:last-child')?.dataset.key === 'youtube:flood12');
    await nico.waitForTimeout(500);
    assert.equal(await nico.$$eval('#niconico .bullet', b => b.length), 0, 'restored comments do not fly again');
    await demo.send({ op: 'message', id: 'fresh', author: 'A', author_id: 'UN1', text: 'あたらしい' });
    await nico.waitForFunction(() => [...document.querySelectorAll('#niconico .bullet')].some(b => b.textContent === 'あたらしい'));
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
