const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {migrateUserData} = require('../dataMigration');
const {migrateSourceList, renameLegacyInputs} = require('../obsSources');
const {OptionalRuntime} = require('../runtime/manager');

function tempDir(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'yuilivekit-'));
  t.after(() => fs.rmSync(dir, {recursive: true, force: true}));
  return dir;
}
function writeFile(file, content = 'x', modifiedAt = null) {
  fs.mkdirSync(path.dirname(file), {recursive: true});
  fs.writeFileSync(file, content);
  if (modifiedAt) fs.utimesSync(file, modifiedAt, modifiedAt);
}

test('moves the legacy folder and everything in it to the new location', t => {
  const appData = tempDir(t), target = path.join(appData, 'YuiLiveKit');
  writeFile(path.join(appData, 'yt-overlay', 'comments.db'), 'comments');
  writeFile(path.join(appData, 'yt-overlay', 'models', 'model.bin'), 'model');

  const result = migrateUserData({appData, target});

  assert.equal(result.status, 'migrated');
  assert.equal(fs.readFileSync(path.join(target, 'comments.db'), 'utf8'), 'comments');
  assert.equal(fs.readFileSync(path.join(target, 'models', 'model.bin'), 'utf8'), 'model');
  assert.equal(fs.existsSync(path.join(appData, 'yt-overlay')), false);
});

test('prefers the legacy folder whose database was used most recently', t => {
  const appData = tempDir(t), target = path.join(appData, 'YuiLiveKit');
  writeFile(path.join(appData, 'yt-overlay', 'comments.db'), 'old', new Date('2026-01-01'));
  writeFile(path.join(appData, 'YouTubeOverlay', 'comments.db'), 'new', new Date('2026-10-01'));

  migrateUserData({appData, target});

  assert.equal(fs.readFileSync(path.join(target, 'comments.db'), 'utf8'), 'new');
  assert.equal(fs.existsSync(path.join(appData, 'yt-overlay', 'comments.db')), true);
});

test('leaves everything alone once the new folder holds a database', t => {
  const appData = tempDir(t), target = path.join(appData, 'YuiLiveKit');
  writeFile(path.join(target, 'comments.db'), 'current');
  writeFile(path.join(appData, 'yt-overlay', 'comments.db'), 'legacy');

  assert.equal(migrateUserData({appData, target}).status, 'current');
  assert.equal(fs.readFileSync(path.join(target, 'comments.db'), 'utf8'), 'current');
  assert.equal(fs.existsSync(path.join(appData, 'yt-overlay', 'comments.db')), true);
});

test('starts fresh when no legacy data exists', t => {
  const appData = tempDir(t);
  fs.mkdirSync(path.join(appData, 'yt-overlay'));
  assert.equal(migrateUserData({appData, target: path.join(appData, 'YuiLiveKit')}).status, 'none');
});

test('keeps a target created before migration by setting it aside', t => {
  const appData = tempDir(t), target = path.join(appData, 'YuiLiveKit');
  writeFile(path.join(target, 'Crashpad', 'settings.dat'), 'early');
  writeFile(path.join(appData, 'yt-overlay', 'comments.db'), 'comments');

  const result = migrateUserData({appData, target, now: () => 123});

  assert.equal(result.setAside, `${target}.before-migration-123`);
  assert.equal(fs.readFileSync(path.join(result.setAside, 'Crashpad', 'settings.dat'), 'utf8'), 'early');
  assert.equal(fs.readFileSync(path.join(target, 'comments.db'), 'utf8'), 'comments');
});

test('restores the target and reports the error when the move fails', t => {
  const appData = tempDir(t), target = path.join(appData, 'YuiLiveKit');
  writeFile(path.join(target, 'early.txt'), 'early');
  writeFile(path.join(appData, 'yt-overlay', 'comments.db'), 'comments');
  const realRename = fs.renameSync;
  t.mock.method(fs, 'renameSync', (from, to) => {
    if (from === path.join(appData, 'yt-overlay')) throw Object.assign(Error('busy'), {code: 'EBUSY'});
    return realRename(from, to);
  });

  assert.throws(() => migrateUserData({appData, target}), {code: 'EBUSY'});
  assert.equal(fs.readFileSync(path.join(target, 'early.txt'), 'utf8'), 'early');
  assert.equal(fs.existsSync(path.join(appData, 'yt-overlay', 'comments.db')), true);
});

test('the private Python search path follows the runtime after the folder moves', t => {
  const root = tempDir(t), folder = path.join(root, 'python');
  writeFile(path.join(folder, 'bootstrap.ready'));
  writeFile(path.join(folder, 'python313._pth'), 'C:\\old\\yt-overlay\\optional-runtime\\backends\\subtitles\n');

  new OptionalRuntime(root, path.join(__dirname, '..'), true);

  const lines = fs.readFileSync(path.join(folder, 'python313._pth'), 'utf8').split('\n');
  assert.ok(lines.includes(path.join(root, 'backends', 'subtitles')));
  assert.ok(lines.includes(path.join(root, 'backends', 'translation')));
  assert.ok(lines.includes('import site'));
  assert.equal(lines.some(line => line.includes('yt-overlay')), false);
});

test('a runtime that was never installed gets no Python search path file', t => {
  const root = tempDir(t);
  new OptionalRuntime(root, path.join(__dirname, '..'), true);
  assert.equal(fs.existsSync(path.join(root, 'python', 'python313._pth')), false);
});

test('saved OBS source lists use the new names and keep custom ones', () => {
  assert.equal(migrateSourceList('YT Overlay - Comments, My Source,YT Overlay - Ohayo'),
    'YuiLiveKit - Comments, My Source, YuiLiveKit - Ohayo');
  assert.equal(migrateSourceList(''), '');
});

test('legacy OBS inputs are renamed unless the new name is already taken', async () => {
  const calls = [];
  const obs = {call: async (request, data) => {
    calls.push([request, data]);
    if (request === 'GetInputList') return {inputs: ['YT Overlay - Comments', 'YT Overlay - Ohayo', 'YuiLiveKit - Ohayo', 'Other'].map(inputName => ({inputName}))};
    return {};
  }};

  const names = await renameLegacyInputs(obs);

  assert.deepEqual(calls.filter(([request]) => request === 'SetInputName').map(([, data]) => data),
    [{inputName: 'YT Overlay - Comments', newInputName: 'YuiLiveKit - Comments'}]);
  assert.ok(names.has('YuiLiveKit - Comments'));
  assert.equal(names.has('YT Overlay - Comments'), false);
  assert.ok(names.has('Other'));
});

test('a failed OBS rename keeps the legacy name usable', async () => {
  const obs = {call: async request => {
    if (request === 'GetInputList') return {inputs: [{inputName: 'YT Overlay - Comments'}]};
    throw Error('rejected');
  }};
  const names = await renameLegacyInputs(obs);
  assert.ok(names.has('YT Overlay - Comments'));
});
