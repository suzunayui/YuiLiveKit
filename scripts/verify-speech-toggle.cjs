// Checks the "start subtitles with chat" switch in the packaged app. A placeholder
// runtime makes the feature count as installed without starting speech recognition.
const {_electron: electron} = require('playwright');
const assert = require('node:assert/strict'), fs = require('node:fs'), path = require('node:path');
const root = path.resolve(__dirname, '..');
const executablePath = path.join(root, 'dist/win-unpacked/YuiLiveKit.exe');
const output = path.join(root, 'test-output/speech-toggle-' + Date.now());
const profile = path.join(output, 'profile'), runtime = path.join(output, 'runtime');
const NEEDS_MICROPHONE = 'マイクを選択すると';

fs.mkdirSync(profile, {recursive: true});
fs.mkdirSync(path.join(runtime, 'python'), {recursive: true});
fs.mkdirSync(path.join(runtime, 'backends/subtitles'), {recursive: true});
fs.writeFileSync(path.join(runtime, 'python/python.exe'), '');
fs.writeFileSync(path.join(runtime, 'backends/subtitles/worker.py'), '');
fs.writeFileSync(path.join(runtime, 'speech.ready.json'), JSON.stringify({version: 1}));
fs.writeFileSync(path.join(profile, 'speech-settings.json'), JSON.stringify({config: {microphone: '', device: 'cpu', model: 'base', threshold: '0.5'}}));
const savedSettings = () => JSON.parse(fs.readFileSync(path.join(profile, 'speech-settings.json'), 'utf8'));

const launch = () => electron.launch({executablePath, env: {...process.env,
  YT_OVERLAY_TEST_PORT: '15013', YT_OVERLAY_TEST_USER_DATA: profile, YT_OVERLAY_TEST_RUNTIME: runtime}});

async function open(desktop) {
  const page = await desktop.firstWindow();
  await page.waitForLoadState('domcontentloaded');
  await desktop.evaluate(() => {
    const original = global.fetch;
    global.fetch = (url, ...args) => String(url).includes('youtube.com') ? new Promise(() => {}) : original(url, ...args);
  });
  await page.locator('#mainTabs [data-tab="addons"]').click();
  await page.locator('#translationEnabled').uncheck();
  await page.locator('#mainTabs [data-tab="speech"]').click();
  await page.locator('#inputTarget').fill('@example');
  return page;
}
const status = page => page.locator('#speechStatus').textContent();

(async () => {
  let desktop = await launch();
  try {
    let page = await open(desktop);
    assert.equal(await page.locator('#speechStartWithChat').isChecked(), true, 'settings from older versions keep starting together');

    await page.locator('#btnStart').click();
    await page.waitForFunction(text => document.querySelector('#speechStatus').textContent.includes(text), NEEDS_MICROPHONE);
    await page.locator('#btnStop').click();

    await page.locator('#speechStartWithChat').uncheck();
    await page.waitForFunction(() => require('electron').ipcRenderer.invoke('speech:get-state').then(state => state.settings.startWithChat === false));
    assert.equal(savedSettings().startWithChat, false);

    await page.locator('#speechReload').click();
    await page.waitForFunction(text => !document.querySelector('#speechStatus').textContent.includes(text), NEEDS_MICROPHONE);
    await page.locator('#btnStart').click();
    await page.waitForFunction(() => document.querySelector('#chatStatus, #btnStop') && !document.querySelector('#btnStop').disabled);
    await page.waitForTimeout(500);
    assert.doesNotMatch(await status(page), new RegExp(NEEDS_MICROPHONE), 'chat start leaves subtitles alone when switched off');
    await page.locator('#btnStop').click();

    await page.locator('#speechModel').selectOption('small');
    await page.waitForFunction(() => require('electron').ipcRenderer.invoke('speech:get-state').then(state => state.settings.config.model === 'small'));
    assert.equal(savedSettings().startWithChat, false, 'saving other settings keeps the switch');
    await page.locator('#speechMaskProfanity').uncheck();
    await page.waitForFunction(() => require('electron').ipcRenderer.invoke('speech:get-state').then(state => state.style.maskProfanity === false));
    assert.equal(savedSettings().startWithChat, false, 'saving the style keeps the switch');

    await desktop.close();
    desktop = await launch();
    page = await open(desktop);
    await page.waitForFunction(() => !document.querySelector('#speechStartWithChat').checked);
    await page.locator('#speechStartWithChat').check();
    await page.waitForFunction(() => require('electron').ipcRenderer.invoke('speech:get-state').then(state => state.settings.startWithChat === true));
    await page.locator('#btnStart').click();
    await page.waitForFunction(text => document.querySelector('#speechStatus').textContent.includes(text), NEEDS_MICROPHONE);
    await page.locator('#btnStop').click();
    console.log('PASS: default on, chat start follows the switch, and the setting survives other saves and a restart.');
  } finally {
    await desktop.close();
  }
})().catch(error => {console.error(error); process.exitCode = 1;});
