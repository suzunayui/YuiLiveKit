const {test} = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto'), fs = require('node:fs'), os = require('node:os'), path = require('node:path');
const {writeChecksums} = require('../scripts/write-checksums.cjs');

test('the checksum file uses the installer name and format YuiLauncher reads', t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'yuilivekit-'));
  t.after(() => fs.rmSync(dir, {recursive: true, force: true}));
  fs.writeFileSync(path.join(dir, 'YuiLiveKit-Setup-2026.1107.1.exe'), 'installer');

  const lines = fs.readFileSync(writeChecksums(dir, '2026.1107.1'), 'utf8').trim().split('\n');

  const expected = crypto.createHash('sha256').update('installer').digest('hex');
  assert.deepEqual(lines, [`${expected}  YuiLiveKit-Setup-2026.1107.1.exe`]);
  assert.match(lines[0], /^([a-fA-F0-9]{64})\s+\*?(.+?)\s*$/);
});

test('package version and installer name follow the launcher rules', () => {
  const pkg = require('../package.json');
  assert.match(pkg.version, /^\d+(\.\d+)*$/);
  assert.equal(pkg.build.artifactName, '${productName}-Setup-${version}.${ext}');
  assert.equal(pkg.build.productName, 'YuiLiveKit');
});

test('a missing installer is reported instead of writing an empty file', t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'yuilivekit-'));
  t.after(() => fs.rmSync(dir, {recursive: true, force: true}));
  assert.throws(() => writeChecksums(dir, '2026.1107.1'), {code: 'ENOENT'});
  assert.equal(fs.existsSync(path.join(dir, 'SHA256SUMS.txt')), false);
});
