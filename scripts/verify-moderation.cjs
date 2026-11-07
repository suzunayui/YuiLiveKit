// Checks the packaged app with an isolated profile: deleted comments, participant
// queue, query plans, second launch and the encrypted OBS password.
const {_electron: electron} = require('playwright');
const assert = require('node:assert/strict'), fs = require('node:fs'), path = require('node:path');
const {spawn} = require('node:child_process');
const root = path.resolve(__dirname, '..');
const executablePath = path.join(root, 'dist/win-unpacked/YuiLiveKit.exe');
const profile = path.join(root, 'test-output/moderation-profile-' + Date.now());
const port = '15012', base = `http://127.0.0.1:${port}`;
const env = {...process.env, YT_OVERLAY_TEST_PORT: port, YT_OVERLAY_TEST_USER_DATA: profile};
const getJson = async url => (await fetch(base + url)).json();

function exitCodeOfSecondLaunch() {
  return new Promise((resolve, reject) => {
    const child = spawn(executablePath, [], {env, stdio: 'ignore'});
    const timer = setTimeout(() => {child.kill(); reject(Error('second launch kept running'));}, 20000);
    child.on('exit', code => {clearTimeout(timer); resolve(code);});
  });
}

(async () => {
  const desktop = await electron.launch({executablePath, env});
  try {
    const page = await desktop.firstWindow();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const store = (method, ...args) => desktop.evaluate(({app}, {method, args}) =>
      process.getBuiltinModule('module').createRequire(app.getAppPath() + '/package.json')('./chatStore')[method](...args), {method, args});

    const now = Date.now();
    const comment = (id, author, channel, offset, text = 'hello ' + id) =>
      ({id, author, text, kind: 'text', video_id: 'video-a', author_channel_id: channel, timestamp_ms: now + offset});
    await store('saveComment', comment('keep', 'Keep', 'channel-keep', 1));
    await store('saveComment', comment('remove-me', 'Spam', 'channel-spam', 2));
    await store('saveComment', comment('ban-1', 'Banned', 'channel-banned', 3));
    await store('saveComment', comment('ban-2', 'Banned', 'channel-banned', 4));

    const overlayOpened = desktop.waitForEvent('window');
    await desktop.evaluate(({BrowserWindow}, url) => {
      new BrowserWindow({show: false, webPreferences: {backgroundThrottling: false}}).loadURL(url);
    }, base + '/');
    const overlay = await overlayOpened;
    overlay.on('pageerror', error => errors.push(error.message));
    await overlay.waitForFunction(() => document.querySelectorAll('.msg').length === 4);
    await page.locator('#mainTabs [data-tab="comments"]').click();
    await page.waitForFunction(() => document.querySelector('#commentList').textContent.includes('hello remove-me'));

    assert.equal(await store('markCommentDeleted', 'remove-me'), 1);
    assert.equal(await store('markAuthorCommentsDeleted', 'channel-banned', 'video-a'), 2);
    await overlay.waitForFunction(() => document.querySelectorAll('.msg').length === 1);
    assert.equal(await overlay.locator('.msg').first().getAttribute('data-comment-id'), 'keep');
    await page.waitForFunction(() => !document.querySelector('#commentList').textContent.includes('hello remove-me'));
    assert.deepEqual((await getJson('/comments')).map(row => row.id), ['keep']);
    assert.deepEqual((await getJson('/api/comments/deleted')).sort(), ['ban-1', 'ban-2', 'remove-me']);

    await store('saveComment', comment('remove-me', 'Spam', 'channel-spam', 2));
    await overlay.reload();
    await overlay.waitForFunction(() => document.querySelectorAll('.msg').length === 1);
    assert.deepEqual((await getJson('/comments')).map(row => row.id), ['keep'], 'a re-delivered comment stays hidden');

    await store('saveComment', comment('join-1', 'Player', 'channel-player', 10, '参加希望です'));
    await store('saveComment', comment('join-2', 'Player', 'channel-player', 11, '参加希望！'));
    let queue = await getJson('/api/participants?videoId=video-a');
    assert.deepEqual(queue.items.filter(item => item.status === 'waiting').map(item => item.id), ['join-1']);
    const post = (url, body) => fetch(base + url, {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)});
    await post('/api/participants/status', {videoId: 'video-a', authorKey: 'channel-player::join-1', status: 'done'});
    queue = await getJson('/api/participants?videoId=video-a');
    assert.deepEqual(queue.items.filter(item => item.status === 'waiting'), [], 'the repeat request does not come back');

    const indexes = await desktop.evaluate(({app}, dbPath) => {
      const require = process.getBuiltinModule('module').createRequire(app.getAppPath() + '/package.json');
      const db = new (require('better-sqlite3'))(dbPath, {readonly: true});
      const plan = sql => db.prepare('EXPLAIN QUERY PLAN ' + sql).all().map(row => row.detail).join(' ');
      const result = {
        names: db.prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_comments_%'").all().map(row => row.name),
        recent: plan('SELECT id FROM comments WHERE deleted_ms IS NULL ORDER BY timestamp_ms DESC, rowid DESC LIMIT 500'),
        viewer: plan("SELECT COALESCE(author_channel_id, author), MIN(timestamp_ms) FROM comments WHERE COALESCE(author_channel_id, author) IN ('a','b') GROUP BY COALESCE(author_channel_id, author)"),
      };
      db.close();
      return result;
    }, path.join(profile, 'comments.db'));
    assert.equal(indexes.names.length, 6);
    assert.match(indexes.recent, /idx_comments_timestamp/);
    assert.match(indexes.viewer, /idx_comments_viewer/);

    await store('saveComment', {...comment('old-name', 'Renamer', null, -100000), video_id: 'video-old'});
    await store('saveComment', comment('linked', 'Renamer', 'channel-renamer', 20));
    await store('saveComment', comment('renamed', 'New Name', 'channel-renamer', 21));
    await store('saveComment', comment('same-name', 'Renamer', 'channel-other', 22));
    await store('saveComment', comment('newcomer', 'Newcomer', 'channel-newcomer', 23));
    const history = Object.fromEntries((await getJson('/comments')).map(row => [row.id, row]));
    assert.equal(history.linked.is_first_time, false, 'history saved by name joins the channel');
    assert.equal(history.renamed.is_first_time, false, 'a new name is the same viewer');
    assert.equal(history.renamed.last_stream_ago, 1);
    assert.equal(history['same-name'].is_first_time, true, 'same name on another channel is another viewer');
    assert.equal(history.newcomer.is_first_time, true);

    const nicoOpened = desktop.waitForEvent('window');
    await desktop.evaluate(({BrowserWindow}, url) => {
      new BrowserWindow({show: false, webPreferences: {backgroundThrottling: false}}).loadURL(url);
    }, base + '/niconico');
    const nico = await nicoOpened;
    nico.on('pageerror', error => errors.push(error.message));
    await nico.waitForLoadState('domcontentloaded');
    await nico.waitForTimeout(800);
    await store('saveComment', comment('nico-gone', 'Spam', 'channel-spam', 100000, 'nico flying text'));
    await nico.waitForFunction(() => [...document.querySelectorAll('.bullet')].some(bullet => bullet.textContent.includes('nico flying text')));
    await store('markCommentDeleted', 'nico-gone');
    await nico.waitForFunction(() => ![...document.querySelectorAll('.bullet')].some(bullet => bullet.textContent.includes('nico flying text')));

    assert.equal(await page.locator('#btnObsOpenLaunchers').count(), 1);

    await store('saveComment', comment('ui-join-1', 'Queue', 'channel-queue', 200000, '参加希望A'));
    await store('saveComment', comment('ui-join-2', 'Queue', 'channel-queue', 200001, '参加希望B'));
    await page.locator('#mainTabs [data-tab="participants"]').click();
    await page.waitForFunction(() => document.querySelector('#participantList').textContent.includes('参加希望A'));
    assert.equal(await page.locator('#participantList').textContent().then(text => text.includes('参加希望B')), false, 'the screen follows the server queue');
    await post('/api/participants/delete', {videoId: 'video-a', authorKey: 'channel-queue::ui-join-1'});
    await page.reload();
    await page.locator('#mainTabs [data-tab="participants"]').click();
    await page.waitForTimeout(1500);
    const shown = await page.locator('#participantList').textContent();
    assert.equal(shown.includes('参加希望A') || shown.includes('参加希望B'), false, 'removed requests stay off the screen after a reload');

    assert.equal((await fetch(base + '/supers')).status, 404, 'removed overlay routes are gone');
    assert.equal((await fetch(base + '/api/concurrent')).status, 404);
    assert.equal((await fetch(base + '/rpgoverlay')).status, 404);
    assert.deepEqual(fs.readdirSync(path.join(profile, 'obs-launchers')).filter(name => /supers|concurrent|effects|underrpg/.test(name)), []);

    await page.evaluate(() => require('electron').ipcRenderer.invoke('obs:save-settings', {host: '127.0.0.1', port: 4455, password: 'test-password'}));
    assert.equal(fs.readFileSync(path.join(profile, 'obs-settings.json'), 'utf8').includes('test-password'), false);
    assert.equal((await page.evaluate(() => require('electron').ipcRenderer.invoke('obs:get-settings'))).password, 'test-password');

    assert.equal(await exitCodeOfSecondLaunch(), 0);
    assert.equal(await page.evaluate(() => 1 + 1), 2, 'the first window survives a second launch');

    assert.deepEqual(errors, []);
    console.log('PASS: deleted comments, participant queue, indexes, removed routes, OBS password and second launch.');
  } finally {
    await desktop.close();
  }
})().catch(error => {console.error(error); process.exitCode = 1;});
