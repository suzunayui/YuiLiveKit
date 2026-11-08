'use strict';

// Collects what a GitHub Release needs into release/: the installer under the public name
// YuiLauncher looks for, and SHA256SUMS.txt, which the launcher checks before installing.
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const { version } = JSON.parse(fs.readFileSync(path.join(root, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const cargo = fs.readFileSync(path.join(root, 'src-tauri', 'Cargo.toml'), 'utf8');
if (!cargo.includes(`version = "${version}"`)) {
  throw new Error(`src-tauri/Cargo.toml must have version ${version}, like tauri.conf.json`);
}
if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`Version ${version} must look like 2026.1108.3`);

const built = path.join(root, 'src-tauri', 'target', 'release', 'bundle', 'nsis', `YuiLiveKit_${version}_x64-setup.exe`);
if (!fs.existsSync(built)) throw new Error(`Build first: ${built} is missing`);
const output = path.join(root, 'release');
const name = `YuiLiveKit-Setup-${version}.exe`;

fs.rmSync(output, { recursive: true, force: true });
fs.mkdirSync(output, { recursive: true });
fs.copyFileSync(built, path.join(output, name));
const sha256 = crypto.createHash('sha256').update(fs.readFileSync(built)).digest('hex');
fs.writeFileSync(path.join(output, 'SHA256SUMS.txt'), `${sha256}  ${name}\n`);
console.log(`release/${name}\nrelease/SHA256SUMS.txt (${sha256})`);
