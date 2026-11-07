// Writes dist/SHA256SUMS.txt, which YuiLauncher checks before running a downloaded installer.
const crypto = require('node:crypto'), fs = require('node:fs'), path = require('node:path');

function writeChecksums(dir, version) {
  const name = `YuiLiveKit-Setup-${version}.exe`;
  const digest = crypto.createHash('sha256').update(fs.readFileSync(path.join(dir, name))).digest('hex');
  const file = path.join(dir, 'SHA256SUMS.txt');
  fs.writeFileSync(file, `${digest}  ${name}\n`);
  return file;
}

if (require.main === module) {
  const {version} = require('../package.json');
  console.log('Wrote', writeChecksums(path.join(__dirname, '../dist'), version));
}
module.exports = {writeChecksums};
