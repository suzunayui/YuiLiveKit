// main.js
const { app, BrowserWindow, dialog, ipcMain, shell, clipboard } = require("electron");
const path = require("path");
const fs = require("fs");
const express = require("express");
const http = require("http");
const { chromium } = require("playwright");
const OBSWebSocket = require("obs-websocket-js").default;
const {
  initChatStore,
  onCommentSaved,
  closeChatStore,
  getDbPath,
  getRecentComments,
  getSupportComments,
  getAuthorFirstSeenMap,
  getVideoFirstSeenMap,
  getAuthorVideoFirstSeenRows,
  getBossState,
  applyBossHit,
  resetBossState,
} = require("./chatStore");

const {
  startLiveChat,
  stopLiveChat,
  getComments,
  resolveVideoId,
} = require("./youtubeChat");

const {LocalTranslation} = require('./translation/local-translation');
const {CommentTranslation} = require('./translation/comments');
const {execFileSync} = require('node:child_process');
const {USER_DATA_NAME, migrateUserData} = require('./dataMigration');
const {migrateSourceList, renameLegacyInputs} = require('./obsSources');
if (process.env.YT_OVERLAY_TEST_USER_DATA) {
  app.setPath('userData', process.env.YT_OVERLAY_TEST_USER_DATA);
} else if (app.isPackaged) {
  // Development runs keep their own folder, so `npm start` never moves installed data.
  const appData = app.getPath('appData');
  const target = path.join(appData, USER_DATA_NAME);
  app.setPath('userData', target);
  try {
    migrateUserData({appData, target});
  } catch (error) {
    dialog.showErrorBox('YuiLiveKit', `以前のバージョンのデータを移行できませんでした。\nYouTubeOverlayを終了してから、もう一度起動してください。\n\n${error.message}`);
    app.exit(1);
  }
}
let commentTranslation = null;
let chatRequested = false;
let chatGeneration = 0;
let currentChatStatus = 'stopped';
function reportChatStatus(status) {
  if(currentChatStatus===status)return;
  currentChatStatus=status;
  if(isWindowAlive())mainWindow.webContents.send('chat:status',status);
}
ipcMain.handle('chat:get-status',()=>currentChatStatus);
function translationSettingsPath() {return path.join(app.getPath('userData'), 'translation-settings.json');}
function normalizeTranslation(raw = {}) {
  return {enabled:raw.enabled !== false, device:/^cuda:\d+$/.test(raw.device) ? raw.device : 'cpu'};
}
function loadTranslationSettings() {
  try {return normalizeTranslation(JSON.parse(fs.readFileSync(translationSettingsPath(), 'utf8')));}
  catch {return {enabled:true, device:'cuda:0'};}
}
function translationState() {return {...(commentTranslation?.state || {}), settings:loadTranslationSettings()};}
function prepareCommentTranslation() {
  const root = __dirname;
  const command = optionalRuntime.python;
  const args = ['-u', optionalRuntime.worker('translation')];
  const runtime = optionalRuntime.llama;
  const worker = new LocalTranslation(command, args, {windowsHide:true, cwd:app.getPath('userData'),
    env:{...process.env, PYTHONUTF8:'1', SUBTITLE_MODEL_DIR:process.env.YT_OVERLAY_MODEL_DIR || path.join(app.getPath('userData'),'models'),
      SUBTITLE_TRANSLATION_RUNTIME:runtime, SUBTITLE_SHARED_MODEL_DIR:path.join(app.getPath('appData'),'live-subtitle-assistant','models')}});
  commentTranslation = new CommentTranslation(worker, state=>{
    if (isWindowAlive()) mainWindow.webContents.send('translation:state', {...state, settings:loadTranslationSettings()});
  });
  onCommentSaved(comment=>commentTranslation.enqueue(comment));
}
ipcMain.handle('translation:get-state', () => translationState());
ipcMain.handle('translation:devices', () => {
  try {
    return execFileSync('nvidia-smi', ['--query-gpu=index,name','--format=csv,noheader'], {windowsHide:true,encoding:'utf8',timeout:5000})
      .trim().split(/\r?\n/).map(line=>{const [index,...name] = line.split(','); return {device:`cuda:${Number(index)}`, name:name.join(',').trim()};});
  } catch {return [];}
});
ipcMain.handle('translation:save', (_event, raw) => {
  const previous = loadTranslationSettings(); const settings = normalizeTranslation(raw);
  fs.writeFileSync(translationSettingsPath(), JSON.stringify(settings));
  if (chatRequested && (previous.enabled !== settings.enabled || previous.device !== settings.device)) {
    if (settings.enabled) startTranslation(settings.device); else commentTranslation.stop();
  }
  return translationState();
});

const OVERLAY_PORT = Number(process.env.YT_OVERLAY_TEST_PORT) || 5000;
const {SpeechSubtitles} = require('./subtitles/service');
const {normalizeStyle} = require('./subtitles/style');
let speech = null;
let speechPendingWithChat=false;
let speechStartedWithChat=false;
const {OptionalRuntime}=require('./runtime/manager');
let optionalRuntime=null;
let shuttingDown=false;
function startTranslation(device){
  if(optionalRuntime.ready('translation')&&!optionalRuntime.state.busy)void commentTranslation.start(device);
  else{commentTranslation.stop();commentTranslation.notify('コメント翻訳は未導入です。「コメント翻訳を導入」を押してください');}
}
function prepareOptionalRuntime(){
  optionalRuntime=new OptionalRuntime(process.env.YT_OVERLAY_TEST_RUNTIME||path.join(app.getPath('userData'),'optional-runtime'),__dirname,app.isPackaged);
  let wasBusy=false;
  optionalRuntime.on('state',state=>{
    if(shuttingDown)return;
    if(wasBusy&&!state.busy&&speech){speech.state.gpuReady=optionalRuntime.ready('cuda');if(optionalRuntime.ready('speech')&&!speech.child)speech.prepare();}
    wasBusy=state.busy;
    if(isWindowAlive())mainWindow.webContents.send('runtime:state',state);
  });
  optionalRuntime.on('installed',kind=>{
    if(shuttingDown)return;
    if(kind==='translation'&&chatRequested&&loadTranslationSettings().enabled)setImmediate(()=>startTranslation(loadTranslationSettings().device));
  });
}
ipcMain.handle('runtime:get-state',()=>optionalRuntime.state);
ipcMain.handle('runtime:install',(_event,kind)=>{
  if(!['speech','translation','cuda'].includes(kind))throw Error('未対応の機能です');
  if(speech.state.running||commentTranslation.state.active)throw Error('字幕と翻訳を停止してから導入してください');
  if(optionalRuntime.state.busy||optionalRuntime.ready(kind))return optionalRuntime.state;
  // Release imported DLLs before pip updates the shared app-private environment.
  speech.stop();commentTranslation.stop();
  void optionalRuntime.install(kind);return optionalRuntime.state;
});
function speechSettingsPath() {return path.join(app.getPath('userData'),'speech-settings.json');}
function normalizeSpeech(raw={}) {
  const config=raw.config || {};
  return {config:{microphone:/^\d+$/.test(String(config.microphone)) ? String(config.microphone) : '',
    device:/^cuda:\d+$/.test(config.device) ? config.device : 'cpu',
    model:['base','small','medium','large-v3'].includes(config.model) ? config.model : 'large-v3',
    threshold:String(Math.max(0.3,Math.min(0.8,Number(config.threshold)||0.5)))},style:normalizeStyle(raw.style)};
}
function loadSpeechSettings() {
  for (const file of [speechSettingsPath(),path.join(app.getPath('appData'),'live-subtitle-assistant','settings.json')]) {
    try {return normalizeSpeech(JSON.parse(fs.readFileSync(file,'utf8')));} catch {}
  }
  return normalizeSpeech();
}
function prepareSpeech() {
  const command=optionalRuntime.python;
  const args=['-u',optionalRuntime.worker('speech')];
  speech=new SpeechSubtitles(command,args,{windowsHide:true,cwd:app.getPath('userData'),
    env:{...process.env,PYTHONUTF8:'1',SUBTITLE_MODEL_DIR:path.join(app.getPath('userData'),'models'),
      SUBTITLE_SHARED_MODEL_DIR:path.join(app.getPath('appData'),'live-subtitle-assistant','models')}},loadSpeechSettings());
  speech.on('state',state=>{
    if(isWindowAlive()) mainWindow.webContents.send('speech:state',state);
    if(speechPendingWithChat&&state.ready)setImmediate(startSpeechWithChat);
  });
  speech.state.gpuReady=optionalRuntime.ready('cuda');
  if(optionalRuntime.ready('speech'))speech.prepare();else speech.state.status='音声字幕は未導入です。「音声字幕を導入」を押してください';
}
ipcMain.handle('speech:get-state',()=>speech.state);
function startSpeech(raw){
  if(optionalRuntime.state.busy)throw Error('導入が終わるまでお待ちください');
  if (!speech.state.ready || speech.state.running) return speech.state;
  const settings=normalizeSpeech(raw);
  if(settings.config.device!=='cpu'&&!optionalRuntime.ready('cuda'))throw Error('「GPU高速化を導入」を押してください。CPUでも使用できます');
  if (!speech.state.devices.some(device=>String(device.id)===settings.config.microphone)) throw new Error('マイクを選択してください');
  if (settings.config.device!=='cpu' && !speech.state.gpus.some(gpu=>`cuda:${gpu.id}`===settings.config.device)) throw new Error('認識デバイスを選択してください');
  fs.writeFileSync(speechSettingsPath(),JSON.stringify(settings)); speech.start(settings); return speech.state;
}
function startSpeechWithChat(){
  if(!speechPendingWithChat||!chatRequested)return;
  if(speech.state.running){speechPendingWithChat=false;return;}
  if(!speech.state.ready)return;
  speechPendingWithChat=false;
  try{
    startSpeech(speech.state.settings);
    speechStartedWithChat=speech.state.running;
  }catch(error){speech.state.status='字幕を開始できません: '+error.message;speech.publish();}
}
function requestSpeechWithChat(){
  if(speech.state.running)return;
  if(!optionalRuntime.ready('speech')||optionalRuntime.state.busy)return;
  if(!speech.state.settings.config.microphone){
    speech.state.status='音声字幕タブでマイクを選択すると、チャット取得開始と一緒に字幕も始まります';speech.publish();return;
  }
  speechPendingWithChat=true;
  startSpeechWithChat();
}
function stopSpeechWithChat(){
  speechPendingWithChat=false;
  if(speechStartedWithChat){speechStartedWithChat=false;speech.stop(optionalRuntime.ready('speech'));}
}
ipcMain.handle('speech:start',(_event,raw)=>{speechPendingWithChat=false;return startSpeech(raw);});
ipcMain.handle('speech:settings',(_event,raw)=>{
  if(speech.state.running)return speech.state;
  const settings=normalizeSpeech(raw);
  fs.writeFileSync(speechSettingsPath(),JSON.stringify(settings));speech.update(settings);return speech.state;
});
ipcMain.handle('speech:stop',()=>{speechPendingWithChat=false;speechStartedWithChat=false;speech.stop(optionalRuntime.ready('speech'));return speech.state;});
ipcMain.handle('speech:reload',()=>{if(!speech.state.running&&optionalRuntime.ready('speech')) speech.stop(true);return speech.state;});
ipcMain.handle('speech:style',(_event,style)=>{
  const settings={...speech.state.settings,style:normalizeStyle(style)};
  fs.writeFileSync(speechSettingsPath(),JSON.stringify(settings));speech.update(settings);return speech.state;
});
ipcMain.handle('speech:clear',()=>{speech.clear();return speech.state;});
ipcMain.handle('speech:copy-url',()=>{const url=`http://127.0.0.1:${OVERLAY_PORT}/subtitles`;clipboard.writeText(url);return url;});
let mainWindow = null;
let boundsSaveTimer = null;
let currentColors = {
  overlay: {
    fontFamily: "Noto Sans JP",
    colorNormal: "#000000",
    alphaNormal: 100,
    colorText: "#ffffff",
    alphaText: 100,
    colorAuthor: "#ffd8f8",
    alphaAuthor: 100,
    fontSize: 22,
    avatarSize: 40,
    fontBold: true,
    shadowEnabled: true,
    colorShadow: "#000000",
    alphaShadow: 90,
    colorMembership: "#1e7d32",
    alphaMembership: 100
  },
  supers: {
    fontFamily: "Noto Sans JP",
    colorNormal: "#000000",
    alphaNormal: 100,
    colorText: "#ffffff",
    alphaText: 100,
    colorAuthor: "#ffd8f8",
    alphaAuthor: 100,
    fontSize: 22,
    avatarSize: 40,
    fontBold: true,
    shadowEnabled: true,
    colorShadow: "#000000",
    alphaShadow: 90,
    colorMembership: "#1e7d32",
    alphaMembership: 100
  },
  nico: {
    fontFamily: "Noto Sans JP",
    colorText: "#ffffff",
    alphaText: 100,
    fontSize: 64,
    fontBold: true,
    scrollDuration: 8,
    shadowEnabled: true,
    colorShadow: "#000000",
    alphaShadow: 100
  }
};
const effectTriggers = [];
const participantsByVideo = new Map();
const ohayoByVideo = new Map();
const PARTICIPANT_KEYWORD = "参加希望";
const OHAYO_KEYWORD = "おは";
const DEFAULT_OBS_SETTINGS = {
  enabled: false,
  host: "127.0.0.1",
  port: 4455,
  password: "",
  sources: "",
  items: ["overlay", "niconico", "participantsVertical", "participantsHorizontal", "ohayo"],
};
const OBS_BROWSER_SOURCE_DEFS = [
  {id:'subtitles',label:'日英音声字幕',inputName:'YuiLiveKit - Subtitles',url:`http://127.0.0.1:${OVERLAY_PORT}/subtitles`,width:1920,height:1080},
  {
    id: "overlay",
    label: "通常コメント",
    inputName: "YuiLiveKit - Comments",
    url: `http://127.0.0.1:${OVERLAY_PORT}/`,
    width: 800,
    height: 600,
  },
  {
    id: "niconico",
    label: "ニコニコ風",
    inputName: "YuiLiveKit - Niconico",
    url: `http://127.0.0.1:${OVERLAY_PORT}/niconico`,
    width: 1920,
    height: 1080,
  },
  {
    id: "participantsVertical",
    label: "参加管理（縦並び）",
    inputName: "YuiLiveKit - Participants Vertical",
    url: `http://127.0.0.1:${OVERLAY_PORT}/participants?layout=vertical`,
    width: 520,
    height: 720,
  },
  {
    id: "participantsHorizontal",
    label: "参加管理（横並び）",
    inputName: "YuiLiveKit - Participants Horizontal",
    url: `http://127.0.0.1:${OVERLAY_PORT}/participants?layout=horizontal`,
    width: 1920,
    height: 300,
  },
  {
    id: "ohayo",
    label: "おはよう",
    inputName: "YuiLiveKit - Ohayo",
    url: `http://127.0.0.1:${OVERLAY_PORT}/ohayo`,
    width: 420,
    height: 140,
  },
];

// ==============================
// 同時接続数の監視（Playwright で watch ページを読む）
// ==============================
let concurrentVideoId = process.env.CONCURRENT_VIDEO_ID || null;
let currentViewers = 0;
let currentLikes = 0;
let concurrentBrowser = null;
let concurrentContext = null;
let concurrentPage = null;
let concurrentLoop = null;
let concurrentStop = false;
const debugConcurrent = process.env.DEBUG_CONCURRENT === "1";
// Default OFF; set DEBUG_UNDERRPG=1 (or add `?debug=1`) to enable.
const debugUnderrpg = process.env.DEBUG_UNDERRPG === "1";

async function closeConcurrentBrowser() {
  if (concurrentPage) {
    try {
      await concurrentPage.close();
    } catch (_) {}
    concurrentPage = null;
  }
  if (concurrentContext) {
    try {
      await concurrentContext.close();
    } catch (_) {}
    concurrentContext = null;
  }
  if (concurrentBrowser) {
    try {
      await concurrentBrowser.close();
    } catch (_) {}
    concurrentBrowser = null;
  }
}

function parseConcurrentFromText(str) {
  if (!str) return null;
  const mJa = str.match(/([\d,\.]+)\s*人が視聴中/);
  if (mJa) return parseInt(mJa[1].replace(/[^\d]/g, ""), 10);
  const mEn = str.match(/([\d,\.]+)\s+watching/);
  if (mEn) return parseInt(mEn[1].replace(/[^\d]/g, ""), 10);
  return null;
}

function parseLikesLabel(label) {
  if (!label) return null;
  const m = label.match(/([\d,\.]+)\s*(?:人|likes?)/i);
  if (m) {
    const n = parseInt(m[1].replace(/[^\d]/g, ""), 10);
    if (!Number.isNaN(n)) return n;
  }
  return null;
}

async function readCountsFromPage(page, fetchLikes = true) {
  try {
    return await page.evaluate(
      (opts) => {
      function parseConcurrent(str) {
        if (!str) return null;
        const mJa = str.match(/([\d,\.]+)\s*人が視聴中/);
        if (mJa) return parseInt(mJa[1].replace(/[^\d]/g, ""), 10);
        const mEn = str.match(/([\d,\.]+)\s+watching/);
        if (mEn) return parseInt(mEn[1].replace(/[^\d]/g, ""), 10);
        return null;
      }
      function parseLikes(str) {
        if (!str) return null;
        const m = str.match(/([\d,\.]+)\s*(?:人|likes?)/i);
        if (m) {
          const n = parseInt(m[1].replace(/[^\d]/g, ""), 10);
          if (!Number.isNaN(n)) return n;
        }
        return null;
      }

      const viewEl = document.querySelector("#view-count");
      const viewStr =
        (viewEl && (viewEl.getAttribute("aria-label") || viewEl.textContent)) ||
        "";
      const viewers = parseConcurrent(viewStr);

        let likes = null;
        if (opts.fetchLikes) {
          const likeBtn = document.querySelector("like-button-view-model button");
          const likeStr =
            (likeBtn &&
              (likeBtn.getAttribute("aria-label") || likeBtn.textContent)) ||
            "";
          likes = parseLikes(likeStr);
        }

        return { viewers, likes };
    },
      { fetchLikes }
    );
  } catch (_) {
    return { viewers: null, likes: null };
  }
}

async function runConcurrentWatcher(videoId) {
  currentViewers = 0;
  concurrentStop = false;

  let lastLikesFetch = 0;
  try {
    concurrentBrowser = await chromium.launch({
      headless: true,
      args: ["--lang=ja-JP"],
    });
    concurrentContext = await concurrentBrowser.newContext({
      viewport: { width: 1280, height: 720 },
      userAgent:
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36",
      locale: "ja-JP",
    });
    concurrentPage = await concurrentContext.newPage();

    await concurrentPage.goto(`https://www.youtube.com/watch?v=${videoId}`, {
      waitUntil: "domcontentloaded",
    });

    // 連続で取れなかった場合は一度だけリロードして再試行
    let retries = 0;
    let last = null;
    let lastLikes = null;
    let likesStaleCount = 0;
    while (!concurrentStop && concurrentVideoId === videoId) {
      const nowTs = Date.now();
      const fetchLikes = nowTs - lastLikesFetch >= 60000;
      const { viewers, likes } = await readCountsFromPage(concurrentPage, fetchLikes);

      if (typeof viewers === "number" && viewers !== last) {
        last = viewers;
        currentViewers = viewers;
        if (debugConcurrent) console.log("同接更新(playwright):", viewers);
        retries = 0;
      }
      if (fetchLikes) {
        if (typeof likes === "number" && likes !== lastLikes) {
          lastLikes = likes;
          currentLikes = likes;
          lastLikesFetch = nowTs;
          if (debugConcurrent) console.log("高評価(playwright):", likes);
          likesStaleCount = 0;
        } else if (typeof likes === "number") {
          likesStaleCount += 1;
          lastLikesFetch = nowTs;
        }
      }

      if (
        typeof viewers !== "number" &&
        typeof likes !== "number" &&
        retries < 2
      ) {
        retries += 1;
        if (debugConcurrent) {
          console.warn("視聴数/高評価が取得できず再読込します (retry:", retries, ")");
        }
        await concurrentPage.reload({ waitUntil: "domcontentloaded" });
        await concurrentPage.waitForTimeout(1000);
      } else if (likesStaleCount >= 6) {
        // 高評価がしばらく変わっていない場合、更新が止まっている可能性があるので再読込
        likesStaleCount = 0;
        if (debugConcurrent) console.warn("高評価が更新されないため再読込します");
        await concurrentPage.reload({ waitUntil: "domcontentloaded" });
        await concurrentPage.waitForTimeout(1000);
      }

      await concurrentPage.waitForTimeout(20000);
    }
  } catch (e) {
    console.warn("同接ウォッチエラー:", e?.message || e);
  } finally {
    await closeConcurrentBrowser();
    concurrentLoop = null;
  }
}

function startConcurrentWatcher(videoId) {
  if (!videoId) return;

  // すでに同じ動画IDで走っているなら再起動しない
  if (concurrentLoop && concurrentVideoId === videoId) {
    return;
  }

  stopConcurrentWatcher();

  concurrentVideoId = videoId;
  if (debugConcurrent) console.log("同接ウォッチ開始(playwright):", videoId);
  concurrentLoop = runConcurrentWatcher(videoId);
}

function stopConcurrentWatcher() {
  concurrentStop = true;
  concurrentVideoId = null;

  if (concurrentLoop) {
    // loop の finally で browser を閉じる
    concurrentLoop.catch(() => {});
  }

  // なるべく早く止めるため即クローズを試みる
  closeConcurrentBrowser();
}

function isWindowAlive() {
  return mainWindow && !mainWindow.isDestroyed();
}

function obsSettingsPath() {
  return path.join(app.getPath("userData"), "obs-settings.json");
}

function normalizeObsSettings(raw = {}) {
  const port = parseInt(raw.port, 10);
  const knownIds = new Set(OBS_BROWSER_SOURCE_DEFS.map((item) => item.id));
  const migratedItems = Array.isArray(raw.items)
    ? raw.items.map((id) => id === "participants" ? "participantsVertical" : id)
    : null;
  const items = migratedItems
    ? migratedItems.filter((id) => knownIds.has(id))
    : DEFAULT_OBS_SETTINGS.items;
  return {
    enabled: raw.enabled === true,
    host: String(raw.host || DEFAULT_OBS_SETTINGS.host).trim() || DEFAULT_OBS_SETTINGS.host,
    port: Number.isFinite(port) && port > 0 ? port : DEFAULT_OBS_SETTINGS.port,
    password: String(raw.password || ""),
    sources: migrateSourceList(raw.sources),
    items: items.length > 0 ? items : DEFAULT_OBS_SETTINGS.items,
  };
}

function loadObsSettings() {
  if (!app.isReady()) return { ...DEFAULT_OBS_SETTINGS };
  try {
    const filePath = obsSettingsPath();
    if (!fs.existsSync(filePath)) return { ...DEFAULT_OBS_SETTINGS };
    return normalizeObsSettings(JSON.parse(fs.readFileSync(filePath, "utf8")));
  } catch (err) {
    console.warn("loadObsSettings error:", err?.message || err);
    return { ...DEFAULT_OBS_SETTINGS };
  }
}

function saveObsSettings(settings) {
  const normalized = normalizeObsSettings(settings);
  if (!app.isReady()) return normalized;
  try {
    fs.writeFileSync(obsSettingsPath(), JSON.stringify(normalized), "utf8");
  } catch (err) {
    console.warn("saveObsSettings error:", err?.message || err);
  }
  return normalized;
}

function obsSourceNames(settings) {
  return String(settings.sources || "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

async function refreshObsBrowserSources(settingsInput, { force = false } = {}) {
  const settings = normalizeObsSettings(settingsInput);
  if (!force && !settings.enabled) {
    return { ok: true, skipped: true, message: "OBS連携はオフです" };
  }

  const obs = new OBSWebSocket();
  const address = `ws://${settings.host}:${settings.port}`;
  try {
    await obs.connect(address, settings.password || undefined);

    const inputNames = await renameLegacyInputs(obs);
    let targets = obsSourceNames(settings);
    if (targets.length === 0) {
      targets = Array.from(inputNames).filter(Boolean);
    }

    const refreshed = [];
    const failed = [];
    for (const inputName of targets) {
      try {
        await obs.call("PressInputPropertiesButton", {
          inputName,
          propertyName: "refreshnocache",
        });
        refreshed.push(inputName);
      } catch (err) {
        failed.push({ inputName, error: err?.message || String(err) });
      }
    }

    return { ok: failed.length === 0, refreshed, failed };
  } finally {
    try {
      await obs.disconnect();
    } catch (_) {}
  }
}

function obsInputSettingsFor(def) {
  return {
    url: def.url,
    width: def.width,
    height: def.height,
    is_local_file: false,
    shutdown: false,
    restart_when_active: true,
  };
}

async function addObsBrowserSources(settingsInput) {
  const settings = normalizeObsSettings(settingsInput);
  const obs = new OBSWebSocket();
  const address = `ws://${settings.host}:${settings.port}`;
  try {
    await obs.connect(address, settings.password || undefined);

    const currentScene = await obs.call("GetCurrentProgramScene");
    const sceneName = currentScene.currentProgramSceneName;
    if (!sceneName) {
      throw new Error("現在のOBSシーンを取得できませんでした");
    }

    const selected = new Set(settings.items);
    const defs = OBS_BROWSER_SOURCE_DEFS.filter((def) => selected.has(def.id));
    const existingInputs = await renameLegacyInputs(obs);

    const added = [];
    const updated = [];
    const failed = [];

    for (const def of defs) {
      try {
        const inputSettings = obsInputSettingsFor(def);
        if (existingInputs.has(def.inputName)) {
          await obs.call("SetInputSettings", {
            inputName: def.inputName,
            inputSettings,
            overlay: true,
          });
          try {
            await obs.call("GetSceneItemId", {
              sceneName,
              sourceName: def.inputName,
            });
          } catch (_) {
            await obs.call("CreateSceneItem", {
              sceneName,
              sourceName: def.inputName,
              sceneItemEnabled: true,
            });
          }
          updated.push(def.inputName);
        } else {
          await obs.call("CreateInput", {
            sceneName,
            inputName: def.inputName,
            inputKind: "browser_source",
            inputSettings,
            sceneItemEnabled: true,
          });
          existingInputs.add(def.inputName);
          added.push(def.inputName);
        }

        await obs.call("PressInputPropertiesButton", {
          inputName: def.inputName,
          propertyName: "refreshnocache",
        });
      } catch (err) {
        failed.push({ inputName: def.inputName, error: err?.message || String(err) });
      }
    }

    return { ok: failed.length === 0, sceneName, added, updated, failed };
  } finally {
    try {
      await obs.disconnect();
    } catch (_) {}
  }
}

function participantVideoKey(videoId) {
  return videoId || "default";
}

function participantAuthorKey(msg) {
  const author = typeof msg?.author === "string" ? msg.author.trim() : "";
  return author || `anonymous:${msg?.id || msg?.timestamp_ms || Date.now()}`;
}

function ohayoAuthorKey(msg) {
  const author = typeof msg?.author === "string" ? msg.author.trim() : "";
  return author || `anonymous:${msg?.id || msg?.timestamp_ms || Date.now()}`;
}

function getParticipantBucket(videoId) {
  const key = participantVideoKey(videoId);
  if (!participantsByVideo.has(key)) {
    participantsByVideo.set(key, new Map());
  }
  return participantsByVideo.get(key);
}

function getOhayoState(videoId) {
  const key = participantVideoKey(videoId);
  if (!ohayoByVideo.has(key)) {
    ohayoByVideo.set(key, { entries: new Map(), adjustment: 0, resetMs: 0 });
  }
  return ohayoByVideo.get(key);
}

function participantStatePath() {
  return path.join(app.getPath("userData"), "participants-state.json");
}

function saveParticipantState() {
  if (!app.isReady()) return;
  try {
    const data = {};
    for (const [videoId, bucket] of participantsByVideo.entries()) {
      data[videoId] = Array.from(bucket.values());
    }
    fs.writeFileSync(participantStatePath(), JSON.stringify(data), "utf8");
  } catch (err) {
    console.warn("saveParticipantState error:", err?.message || err);
  }
}

function loadParticipantState() {
  if (!app.isReady()) return;
  try {
    const filePath = participantStatePath();
    if (!fs.existsSync(filePath)) return;
    const data = JSON.parse(fs.readFileSync(filePath, "utf8"));
    participantsByVideo.clear();
    for (const [videoId, items] of Object.entries(data || {})) {
      const bucket = getParticipantBucket(videoId);
      for (const item of Array.isArray(items) ? items : []) {
        if (!item || !item.author_key) continue;
        if (!item.author_id) item.author_id = item.author_key;
        bucket.set(item.author_key, item);
      }
    }
  } catch (err) {
    console.warn("loadParticipantState error:", err?.message || err);
  }
}

function ohayoStatePath() {
  return path.join(app.getPath("userData"), "ohayo-state.json");
}

function saveOhayoState() {
  if (!app.isReady()) return;
  try {
    const data = {};
    for (const [videoId, state] of ohayoByVideo.entries()) {
      data[videoId] = {
        adjustment: Number(state.adjustment) || 0,
        reset_ms: Number(state.resetMs) || 0,
        entries: Array.from(state.entries.values()),
      };
    }
    fs.writeFileSync(ohayoStatePath(), JSON.stringify(data), "utf8");
  } catch (err) {
    console.warn("saveOhayoState error:", err?.message || err);
  }
}

function loadOhayoState() {
  if (!app.isReady()) return;
  try {
    const filePath = ohayoStatePath();
    if (!fs.existsSync(filePath)) return;
    const data = JSON.parse(fs.readFileSync(filePath, "utf8"));
    ohayoByVideo.clear();
    for (const [videoId, rawState] of Object.entries(data || {})) {
      const state = getOhayoState(videoId);
      state.adjustment = Number(rawState?.adjustment) || 0;
      state.resetMs = Number(rawState?.reset_ms) || 0;
      for (const entry of Array.isArray(rawState?.entries) ? rawState.entries : []) {
        if (!entry || !entry.author_key) continue;
        state.entries.set(entry.author_key, entry);
      }
    }
  } catch (err) {
    console.warn("loadOhayoState error:", err?.message || err);
  }
}

function participantItems(videoId) {
  return Array.from(getParticipantBucket(videoId).values())
    .filter((item) => item.status !== "deleted")
    .sort((a, b) => {
      const aOrder = Number.isFinite(a.queue_order) ? a.queue_order : (a.accepted_ms || 0);
      const bOrder = Number.isFinite(b.queue_order) ? b.queue_order : (b.accepted_ms || 0);
      return aOrder - bOrder;
    });
}

function upsertParticipantFromComment(msg) {
  if (!msg || msg.kind !== "text") return false;
  const text = typeof msg.text === "string" ? msg.text : "";
  if (!text.includes(PARTICIPANT_KEYWORD)) return false;

  const videoId = msg.video_id || "default";
  const authorId = participantAuthorKey(msg);
  const id = String(msg.id || `${msg.timestamp_ms || 0}_${msg.author || ""}_${text}`);
  const requestKey = `${authorId}::${id}`;
  const bucket = getParticipantBucket(videoId);
  if (bucket.has(requestKey)) return false;
  for (const item of bucket.values()) {
    const itemAuthorId = item.author_id || item.author_key;
    if (itemAuthorId === authorId && item.status === "waiting") {
      return false;
    }
  }

  bucket.set(requestKey, {
    id,
    video_id: videoId,
    author_key: requestKey,
    author_id: authorId,
    author: msg.author || "(no name)",
    icon: msg.icon || "",
    text,
    accepted_ms: Number(msg.timestamp_ms) || Date.now(),
    queue_order: Number(msg.timestamp_ms) || Date.now(),
    status: "waiting",
  });
  saveParticipantState();
  return true;
}

function syncParticipantsFromComments(rows) {
  if (!Array.isArray(rows)) return;
  for (const row of rows) {
    upsertParticipantFromComment(row);
  }
}

function upsertOhayoFromComment(msg) {
  if (!msg || msg.kind !== "text") return false;
  const text = typeof msg.text === "string" ? msg.text : "";
  if (!text.includes(OHAYO_KEYWORD)) return false;

  const videoId = msg.video_id || "default";
  const authorKey = ohayoAuthorKey(msg);
  const state = getOhayoState(videoId);
  const ts = Number(msg.timestamp_ms) || Date.now();
  if (state.resetMs && ts <= state.resetMs) return false;
  if (state.entries.has(authorKey)) return false;

  state.entries.set(authorKey, {
    id: String(msg.id || `${msg.timestamp_ms || 0}_${msg.author || ""}_${text}`),
    video_id: videoId,
    author_key: authorKey,
    author: msg.author || "(no name)",
    icon: msg.icon || "",
    text,
    accepted_ms: ts,
  });
  saveOhayoState();
  return true;
}

function syncOhayoFromComments(rows) {
  if (!Array.isArray(rows)) return;
  for (const row of rows) {
    upsertOhayoFromComment(row);
  }
}

function ohayoPayload(videoId) {
  const state = getOhayoState(videoId);
  const detected = state.entries.size;
  const adjustment = Number(state.adjustment) || 0;
  return {
    ok: true,
    videoId: participantVideoKey(videoId),
    keyword: OHAYO_KEYWORD,
    detected,
    adjustment,
    reset_ms: Number(state.resetMs) || 0,
    count: Math.max(0, detected + adjustment),
    entries: Array.from(state.entries.values()).sort(
      (a, b) => (a.accepted_ms || 0) - (b.accepted_ms || 0)
    ),
  };
}

function latestVideoIdFromRows(rows) {
  let latest = null;
  for (const row of rows || []) {
    if (!row || !row.video_id) continue;
    const ts = Number(row.timestamp_ms) || 0;
    if (!latest || ts > latest.ts) latest = { id: row.video_id, ts };
  }
  return latest ? latest.id : null;
}

function latestParticipantVideoId() {
  let latest = null;
  for (const [videoId, bucket] of participantsByVideo.entries()) {
    for (const item of bucket.values()) {
      const ts = Number(item.accepted_ms) || 0;
      if (!latest || ts > latest.ts) latest = { id: videoId, ts };
    }
  }
  return latest ? latest.id : null;
}

function latestOhayoVideoId() {
  let latest = null;
  for (const [videoId, state] of ohayoByVideo.entries()) {
    for (const item of state.entries.values()) {
      const ts = Number(item.accepted_ms) || 0;
      if (!latest || ts > latest.ts) latest = { id: videoId, ts };
    }
  }
  return latest ? latest.id : null;
}

/**
 * オーバーレイ用 HTTP サーバ (http://127.0.0.1:5000/...) を起動
 */
function createOverlayServer() {
  const srv = express();

  srv.get('/subtitles',(_req,res)=>res.sendFile(path.join(__dirname,'subtitle-overlay.html')));
  srv.get('/subtitle-events',(req,res)=>{
    res.setHeader('Content-Type','text/event-stream');res.setHeader('Cache-Control','no-cache');res.setHeader('Connection','keep-alive');res.flushHeaders();
    // Level events are only needed by the controls, not the browser overlay.
    const send=()=>res.write('data: '+JSON.stringify({subtitle:speech.state.subtitle,style:speech.state.style})+'\n\n');
    let previous='';
    const changed=()=>{const key=JSON.stringify([speech.state.subtitle,speech.state.style]);if(key!==previous){previous=key;send();}};
    speech.on('state',changed);changed();
    const heartbeat=setInterval(()=>res.write(': heartbeat\n\n'),15000);
    req.on('close',()=>{clearInterval(heartbeat);speech.off('state',changed);});
  });

  // Serve static assets (JS modules, CSS, etc.)
  srv.use(express.static(__dirname));

  srv.get("/", (req, res) => {
    res.sendFile(path.join(__dirname, "overlay.html"));
  });

  srv.get("/niconico", (req, res) => {
    res.sendFile(path.join(__dirname, "niconico.html"));
  });

  srv.get("/supers", (req, res) => {
    res.sendFile(path.join(__dirname, "supers.html"));
  });

  srv.get("/participants", (req, res) => {
    res.sendFile(path.join(__dirname, "participants.html"));
  });

  srv.get("/ohayo", (req, res) => {
    res.sendFile(path.join(__dirname, "ohayo.html"));
  });

  srv.get(["/concurrent", "/overlay/concurrent"], (req, res) => {
    res.sendFile(path.join(__dirname, "concurrent.html"));
  });

  srv.get("/effects", (req, res) => {
    res.sendFile(path.join(__dirname, "effects.html"));
  });

  srv.get("/rpgoverlay", (req, res) => {
    res.setHeader("Cache-Control", "no-store");
    res.sendFile(path.join(__dirname, "underrpg.html"));
  });

  async function fetchOverlayRows(limit, afterMs) {
    const LIM = Number.isFinite(limit) && limit > 0 ? Math.min(limit, 500) : 50;
    const fetchLimit = Math.min(500, Math.max(50, LIM * 3));

    const normalizeIconUrl = (icon) => {
      if (typeof icon !== "string") return null;
      const s = icon.trim();
      if (!s) return null;
      if (s.startsWith("//")) return "https:" + s;
      if (s.startsWith("http://")) return "https://" + s.slice("http://".length);
      return s;
    };

    const normalize = (r) => {
      const ts = Number(r.timestamp_ms) || 0;
      const author = typeof r.author === "string" ? r.author : "";
      const text = typeof r.text === "string" ? r.text : "";
      const id = r.id != null ? String(r.id) : `${ts}_${author}_${text}`;
      return { id, icon: normalizeIconUrl(r.icon), author, text, timestamp_ms: ts };
    };

    try {
      const rows = await getRecentComments(fetchLimit);
      const uniq = new Map();
      for (const r of rows) {
        const item = normalize(r);
        if (!uniq.has(item.id)) uniq.set(item.id, item);
      }
      let out = Array.from(uniq.values()).sort((a, b) => a.timestamp_ms - b.timestamp_ms);
      if (Number.isFinite(afterMs) && afterMs > 0) {
        out = out.filter((r) => r.timestamp_ms > afterMs);
      }
      return out.slice(-LIM);
    } catch (_) {
      const fallback = (getComments() || []).map(normalize);
      const uniq = new Map();
      for (const r of fallback) {
        if (!uniq.has(r.id)) uniq.set(r.id, r);
      }
      let out = Array.from(uniq.values()).sort((a, b) => a.timestamp_ms - b.timestamp_ms);
      if (Number.isFinite(afterMs) && afterMs > 0) {
        out = out.filter((r) => r.timestamp_ms > afterMs);
      }
      return out.slice(-LIM);
    }
  }

  srv.get("/api/recent", async (req, res) => {
    const reqDebug = debugUnderrpg || req.query.debug === "1";
    const limit = parseInt(req.query.limit, 10);
    const rows = await fetchOverlayRows(limit, null);
    if (reqDebug) {
      const withIcon = rows.filter((r) => typeof r.icon === "string" && r.icon.length > 0).length;
      console.log(
        `[underrpg] /api/recent limit=${Number.isFinite(limit) ? limit : "?"} rows=${rows.length} withIcon=${withIcon}`
      );
      if (rows.length > 0) {
        console.log("[underrpg] /api/recent sample:", {
          id: rows[rows.length - 1].id,
          author: rows[rows.length - 1].author,
          icon: rows[rows.length - 1].icon,
          timestamp_ms: rows[rows.length - 1].timestamp_ms,
        });
      }
    }
    res.json(
      rows.map(({ id, icon, author, text, timestamp_ms }) => ({ id, icon, author, text, timestamp_ms }))
    );
  });

  srv.get("/api/events", (req, res) => {
    const reqDebug = debugUnderrpg || req.query.debug === "1";
    res.setHeader("Content-Type", "text/event-stream; charset=utf-8");
    res.setHeader("Cache-Control", "no-cache, no-transform");
    res.setHeader("Connection", "keep-alive");
    res.setHeader("X-Accel-Buffering", "no");
    if (typeof res.flushHeaders === "function") res.flushHeaders();

    let lastAfterMs = parseInt(req.query.after, 10);
    if (!Number.isFinite(lastAfterMs)) lastAfterMs = 0;

    if (reqDebug) {
      console.log("[underrpg] /api/events connect", {
        ip: req.ip,
        ua: req.headers["user-agent"],
        after: lastAfterMs,
      });
    }

    let sentTotal = 0;
    let lastLogTs = 0;
    let loggedNullIcon = false;

    const timer = setInterval(async () => {
      try {
        const rows = await fetchOverlayRows(50, lastAfterMs);
        const now = Date.now();
        if (reqDebug && now - lastLogTs > 5000) {
          const withIcon = rows.filter((r) => typeof r.icon === "string" && r.icon.length > 0).length;
          console.log(
            `[underrpg] /api/events tick rows=${rows.length} withIcon=${withIcon} after=${lastAfterMs} sentTotal=${sentTotal}`
          );
          lastLogTs = now;
        }
        for (const row of rows) {
          if (reqDebug && !loggedNullIcon && !row.icon) {
            loggedNullIcon = true;
            console.log("[underrpg] /api/events null icon sample:", {
              id: row.id,
              author: row.author,
              timestamp_ms: row.timestamp_ms,
            });
          }
          if (row.timestamp_ms > lastAfterMs) lastAfterMs = row.timestamp_ms;
          res.write(
            `data: ${JSON.stringify({
              id: row.id,
              icon: row.icon,
              author: row.author,
              text: row.text,
              timestamp_ms: row.timestamp_ms,
            })}\n\n`
          );
          sentTotal += 1;
        }
      } catch (_) {}
    }, 500);

    const keepAlive = setInterval(() => {
      try {
        res.write(": keepalive\n\n");
      } catch (_) {}
    }, 20000);

    req.on("close", () => {
      clearInterval(timer);
      clearInterval(keepAlive);
      if (reqDebug) {
        console.log("[underrpg] /api/events close", { sentTotal, lastAfterMs });
      }
    });
  });

  srv.get("/api/boss/state", async (req, res) => {
    const baseHp = parseInt(req.query.baseHp, 10);
    const scale = Number(req.query.scale);
    const st = await getBossState({ baseHp, scale });
    if (!st) return res.status(500).json({ ok: false, error: "db" });
    res.json({ ok: true, state: st });
  });

  srv.post("/api/boss/hit", express.json(), async (req, res) => {
    const damage = parseInt(req.body?.damage, 10);
    const baseHp = parseInt(req.body?.baseHp, 10);
    const scale = Number(req.body?.scale);
    const st = await applyBossHit({ damage, baseHp, scale });
    if (!st) return res.status(500).json({ ok: false, error: "db" });
    res.json({ ok: true, state: st });
  });

  srv.post("/api/boss/reset", express.json(), async (req, res) => {
    const baseHp = parseInt(req.body?.baseHp, 10);
    const st = await resetBossState({ baseHp });
    if (!st) return res.status(500).json({ ok: false, error: "db" });
    res.json({ ok: true, state: st });
  });

  srv.get("/api/participants", async (req, res) => {
    let videoId = typeof req.query.videoId === "string" ? req.query.videoId : "";
    try {
      const rows = await getRecentComments(500);
      syncParticipantsFromComments(rows);
      if (!videoId) {
        videoId = latestVideoIdFromRows(rows) || latestParticipantVideoId() || "default";
      }
    } catch (e) {
      console.warn("participants sync error:", e?.message || e);
      if (!videoId) videoId = latestParticipantVideoId() || "default";
    }
    res.json({
      ok: true,
      videoId,
      keyword: PARTICIPANT_KEYWORD,
      items: participantItems(videoId),
    });
  });

  srv.post("/api/participants/status", express.json(), (req, res) => {
    const videoId = req.body?.videoId || "default";
    const authorKey = String(req.body?.authorKey || "");
    const status = req.body?.status === "done" ? "done" : "waiting";
    const item = getParticipantBucket(videoId).get(authorKey);
    if (!item) return res.status(404).json({ ok: false, error: "not found" });
    item.status = status;
    item.updated_ms = Date.now();
    saveParticipantState();
    res.json({ ok: true, item });
  });

  srv.post("/api/participants/reorder", express.json(), (req, res) => {
    const videoId = req.body?.videoId || "default";
    const authorKeys = Array.isArray(req.body?.authorKeys)
      ? req.body.authorKeys.map(String)
      : [];
    const bucket = getParticipantBucket(videoId);
    const current = participantItems(videoId);
    const waiting = current.filter((item) => item.status === "waiting");
    const requested = new Set(authorKeys);
    const ordered = [
      ...authorKeys.map((key) => bucket.get(key)).filter((item) => item?.status === "waiting"),
      ...waiting.filter((item) => !requested.has(item.author_key)),
    ];
    const orderedAll = [
      ...ordered,
      ...current.filter((item) => item.status !== "waiting"),
    ];
    const baseOrder = current.reduce((min, item) => {
      const value = Number.isFinite(item.queue_order) ? item.queue_order : (item.accepted_ms || Date.now());
      return Math.min(min, value);
    }, Date.now());
    orderedAll.forEach((item, index) => {
      item.queue_order = baseOrder + index;
      item.updated_ms = Date.now();
    });
    saveParticipantState();
    res.json({ ok: true, items: participantItems(videoId) });
  });

  srv.post("/api/participants/delete", express.json(), (req, res) => {
    const videoId = req.body?.videoId || "default";
    const authorKey = String(req.body?.authorKey || "");
    const bucket = getParticipantBucket(videoId);
    const item = bucket.get(authorKey);
    if (item) {
      item.status = "deleted";
      item.updated_ms = Date.now();
      saveParticipantState();
    }
    res.json({ ok: true });
  });

  srv.post("/api/participants/clear", express.json(), (req, res) => {
    const videoId = req.body?.videoId || "default";
    const mode = req.body?.mode === "done" ? "done" : "all";
    const bucket = getParticipantBucket(videoId);
    if (mode === "done") {
      for (const item of bucket.values()) {
        if (item.status === "done") {
          item.status = "deleted";
          item.updated_ms = Date.now();
        }
      }
    } else {
      for (const item of bucket.values()) {
        item.status = "deleted";
        item.updated_ms = Date.now();
      }
    }
    saveParticipantState();
    res.json({ ok: true, items: participantItems(videoId) });
  });

  srv.get("/api/ohayo", async (req, res) => {
    let videoId = typeof req.query.videoId === "string" ? req.query.videoId : "";
    try {
      const rows = await getRecentComments(500);
      syncOhayoFromComments(rows);
      if (!videoId) {
        videoId = latestVideoIdFromRows(rows) || latestOhayoVideoId() || "default";
      }
    } catch (e) {
      console.warn("ohayo sync error:", e?.message || e);
      if (!videoId) videoId = latestOhayoVideoId() || "default";
    }
    res.json(ohayoPayload(videoId));
  });

  srv.post("/api/ohayo/adjust", express.json(), (req, res) => {
    const videoId = req.body?.videoId || "default";
    const delta = parseInt(req.body?.delta, 10);
    const state = getOhayoState(videoId);
    state.adjustment += Number.isFinite(delta) ? delta : 0;
    const minAdjustment = -state.entries.size;
    if (state.adjustment < minAdjustment) state.adjustment = minAdjustment;
    saveOhayoState();
    res.json(ohayoPayload(videoId));
  });

  srv.post("/api/ohayo/reset", express.json(), (req, res) => {
    const videoId = req.body?.videoId || "default";
    const state = getOhayoState(videoId);
    state.entries.clear();
    state.adjustment = 0;
    state.resetMs = Date.now();
    saveOhayoState();
    res.json(ohayoPayload(videoId));
  });

  srv.get('/api/support-comments',async(req,res)=>{
    const rows=await getSupportComments(String(req.query.video||''));
    res.json(commentTranslation.decorate(rows));
  });
  srv.get("/comments", async (req, res) => {
    const limit = parseInt(req.query.limit, 10);
    const after = parseInt(req.query.after, 10);
    const afterMs = Number.isFinite(after) ? after : null;
    const LIM = Number.isFinite(limit) && limit > 0 ? Math.min(limit, 500) : null;
    const fetchLimit = LIM ?? 500; // after フィルタ用に多めに取得
    try {
      const rows = await getRecentComments(fetchLimit);
      if (rows.length > 0) {
          // id 重複を排除し、timestamp_ms 昇順に整列
          const uniq = new Map();
          for (const r of rows) {
            const key = `${r.timestamp_ms || 0}_${r.author || ""}_${r.text || ""}`;
            if (!uniq.has(key)) uniq.set(key, r);
          }
          let out = Array.from(uniq.values()).sort(
            (a, b) => (a.timestamp_ms || 0) - (b.timestamp_ms || 0)
          );
          if (afterMs != null) {
            out = out.filter((r) => (r.timestamp_ms || 0) > afterMs);
          }
          if (LIM) {
            out = out.slice(-LIM);
          }
          const authorMap = getAuthorFirstSeenMap(out.map((r) => r.author));
          const videoFirstMap = getVideoFirstSeenMap();
          const videoIndexMap = new Map(
            Array.from(videoFirstMap.entries())
              .sort((a, b) => a[1] - b[1])
              .map(([videoId], idx) => [videoId, idx])
          );
          const authorVideoRows = getAuthorVideoFirstSeenRows(
            out.map((r) => r.author)
          );
          const authorVideoMap = new Map();
          for (const row of authorVideoRows) {
            if (!row || !row.author || !row.video_id) continue;
            const idx = videoIndexMap.get(row.video_id);
            if (idx == null) continue;
            const list = authorVideoMap.get(row.author) || [];
            list.push({
              videoId: row.video_id,
              index: idx,
              firstTs: Number(row.first_ts) || 0,
            });
            authorVideoMap.set(row.author, list);
          }
          for (const list of authorVideoMap.values()) {
            list.sort((a, b) => a.index - b.index);
          }
          const withMeta = out.map((r) => {
            const ts = Number(r.timestamp_ms) || 0;
            const isFirst =
              !!r.author && ts === (authorMap.get(r.author) || 0);
            let lastStreamFirstTs = null;
            let lastStreamAgo = null;
            const currentIndex = r.video_id
              ? videoIndexMap.get(r.video_id)
              : null;
            if (!isFirst && currentIndex != null && r.author) {
              const list = authorVideoMap.get(r.author) || [];
              for (let i = list.length - 1; i >= 0; i -= 1) {
                const entry = list[i];
                if (entry.index < currentIndex) {
                  lastStreamFirstTs = entry.firstTs || null;
                  lastStreamAgo = currentIndex - entry.index;
                  break;
                }
              }
            }
            return {
              ...r,
              is_first_time: isFirst,
              last_stream_first_ts: lastStreamFirstTs,
              last_stream_ago: lastStreamAgo,
            };
          });
          syncParticipantsFromComments(withMeta);
          syncOhayoFromComments(withMeta);
          return res.json(commentTranslation ? commentTranslation.decorate(withMeta) : withMeta);
      }
    } catch (e) {
      console.warn("getRecentComments error:", e?.message || e);
    }
    // DB が空 / 読み込み失敗時はインメモリを返す
    const fallback = getComments();
    let trimmed = LIM ? fallback.slice(-LIM) : fallback;
    if (afterMs != null) {
      trimmed = trimmed.filter((r) => (r.timestamp_ms || 0) > afterMs);
    }
    const uniq = new Map();
    for (const r of trimmed) {
      const key = `${r.timestamp_ms || 0}_${r.author || ""}_${r.text || ""}`;
      if (!uniq.has(key)) uniq.set(key, r);
    }
    const fallbackRows = Array.from(uniq.values());
    const authorMap = getAuthorFirstSeenMap(fallbackRows.map((r) => r.author));
    const videoFirstMap = getVideoFirstSeenMap();
    const videoIndexMap = new Map(
      Array.from(videoFirstMap.entries())
        .sort((a, b) => a[1] - b[1])
        .map(([videoId], idx) => [videoId, idx])
    );
    const authorVideoRows = getAuthorVideoFirstSeenRows(
      fallbackRows.map((r) => r.author)
    );
    const authorVideoMap = new Map();
    for (const row of authorVideoRows) {
      if (!row || !row.author || !row.video_id) continue;
      const idx = videoIndexMap.get(row.video_id);
      if (idx == null) continue;
      const list = authorVideoMap.get(row.author) || [];
      list.push({
        videoId: row.video_id,
        index: idx,
        firstTs: Number(row.first_ts) || 0,
      });
      authorVideoMap.set(row.author, list);
    }
    for (const list of authorVideoMap.values()) {
      list.sort((a, b) => a.index - b.index);
    }
    const withMeta = fallbackRows.map((r) => {
      const ts = Number(r.timestamp_ms) || 0;
      const isFirst =
        !!r.author && ts === (authorMap.get(r.author) || 0);
      let lastStreamFirstTs = null;
      let lastStreamAgo = null;
      const currentIndex = r.video_id
        ? videoIndexMap.get(r.video_id)
        : null;
      if (!isFirst && currentIndex != null && r.author) {
        const list = authorVideoMap.get(r.author) || [];
        for (let i = list.length - 1; i >= 0; i -= 1) {
          const entry = list[i];
          if (entry.index < currentIndex) {
            lastStreamFirstTs = entry.firstTs || null;
            lastStreamAgo = currentIndex - entry.index;
            break;
          }
        }
      }
      return {
        ...r,
        is_first_time: isFirst,
        last_stream_first_ts: lastStreamFirstTs,
        last_stream_ago: lastStreamAgo,
      };
    });
    syncParticipantsFromComments(withMeta);
    syncOhayoFromComments(withMeta);
    res.json(commentTranslation ? commentTranslation.decorate(withMeta) : withMeta);
  });

  // エフェクト手動トリガー API
  const triggerRouter = express.Router();
  triggerRouter.use(express.json());
  triggerRouter.use((req, res, next) => {
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Methods", "GET,POST,OPTIONS");
    res.setHeader("Access-Control-Allow-Headers", "Content-Type");
    if (req.method === "OPTIONS") return res.sendStatus(204);
    next();
  });
  triggerRouter.post("/", (req, res) => {
    const t = (req.body && req.body.type) || "";
    const allowed = new Set(["firework", "snow", "heart", "star", "explosion", "tikuwa", "sirakaba"]);
    if (!allowed.has(t)) {
      return res.status(400).json({ ok: false, error: "invalid type" });
    }
    effectTriggers.push({ type: t, at: Date.now() });
    res.json({ ok: true });
  });
  triggerRouter.get("/pull", (req, res) => {
    const out = effectTriggers.splice(0, effectTriggers.length);
    res.json(out);
  });
  srv.use("/trigger-effect", triggerRouter);

  srv.get("/api/concurrent", (req, res) => {
    res.json({
      viewers: currentViewers,
      likes: currentLikes,
      videoId: concurrentVideoId,
    });
  });

  // オーバーレイ設定を取得するエンドポイント
  srv.get("/settings/colors", (req, res) => {
    res.json(currentColors.overlay);
  });

  // Supers 設定を取得するエンドポイント
  srv.get("/settings/supers", (req, res) => {
    res.json(currentColors.supers);
  });

  // ニコニコ風設定を取得するエンドポイント
  srv.get("/settings/nico", (req, res) => {
    res.json(currentColors.nico);
  });

  const server = http.createServer(srv);
  server.listen(OVERLAY_PORT, "127.0.0.1", () => {
    console.log("Overlay server listening on http://127.0.0.1:5000/");
  });
}

/**
 * OBS にドラッグ & ドロップする用の HTML を自動生成
 * 戻り値: 作成したディレクトリのパス
 */
function createObsLauncherFiles() {
  // ユーザーデータ配下に置く: 例) C:\Users\xxx\AppData\Roaming\[app名]\obs-launchers
  const baseDir = path.join(app.getPath("userData"), "obs-launchers");
  fs.mkdirSync(baseDir, { recursive: true });

  const makeLauncher = (filename, url) => {
    const filePath = path.join(baseDir, filename);
    const html = `<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<title>OBS Browser Launcher</title>
<style>
  html, body, iframe {
    margin: 0;
    padding: 0;
    width: 100%;
    height: 100%;
    background: transparent;
    overflow: hidden;
  }
</style>
</head>
<body>
  <!-- このHTMLファイルを OBS にドラッグ＆ドロップすると -->
  <!-- 中の iframe が ${url} を表示します -->
  <iframe src="${url}" frameborder="0"></iframe>
</body>
</html>
`;
    fs.writeFileSync(filePath, html, "utf8");
    return filePath;
  };

  makeLauncher("overlay-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/`);
  makeLauncher('subtitles-launcher.html', `http://127.0.0.1:${OVERLAY_PORT}/subtitles`);
  makeLauncher("niconico-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/niconico`);
  makeLauncher("supers-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/supers`);
  makeLauncher("participants-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/participants`);
  makeLauncher("participants-horizontal-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/participants?layout=horizontal`);
  makeLauncher("ohayo-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/ohayo`);
  makeLauncher("concurrent-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/concurrent`);
  makeLauncher("effects-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/effects`);
  makeLauncher("underrpg-launcher.html", `http://127.0.0.1:${OVERLAY_PORT}/rpgoverlay`);

  console.log("📁 OBS launcher files created in:", baseDir);
  return baseDir;
}

/**
 * 設定用ウィンドウ
 */
function createMainWindow(launchersDir) {
  const loadWindowState = () => {
    try {
      const statePath = path.join(app.getPath("userData"), "window-state.json");
      if (!fs.existsSync(statePath)) return null;
      const raw = fs.readFileSync(statePath, "utf8");
      const data = JSON.parse(raw);
      if (!data || typeof data !== "object") return null;
      const width = parseInt(data.width, 10);
      const height = parseInt(data.height, 10);
      const x = parseInt(data.x, 10);
      const y = parseInt(data.y, 10);
      if (!Number.isFinite(width) || !Number.isFinite(height)) return null;
      return {
        width: Math.max(360, width),
        height: Math.max(540, height),
        x: Number.isFinite(x) ? x : undefined,
        y: Number.isFinite(y) ? y : undefined,
      };
    } catch (_) {
      return null;
    }
  };

  const saveWindowState = () => {
    if (!isWindowAlive()) return;
    try {
      const statePath = path.join(app.getPath("userData"), "window-state.json");
      const bounds = mainWindow.getBounds();
      fs.writeFileSync(statePath, JSON.stringify(bounds), "utf8");
    } catch (_) {}
  };

  const saved = loadWindowState();

  mainWindow = new BrowserWindow({
    show: !process.env.YT_OVERLAY_TEST_USER_DATA,
    width: saved?.width || 1200,
    height: saved?.height || 620,
    x: saved?.x,
    y: saved?.y,
    resizable: true,
    minWidth: 360,
    minHeight: 540,
    webPreferences: {
      nodeIntegration: true,
      contextIsolation: false,
    },
  });

  mainWindow.loadFile(path.join(__dirname, "settings.html"));

  mainWindow.on("closed", () => {
    if (boundsSaveTimer) {
      clearTimeout(boundsSaveTimer);
      boundsSaveTimer = null;
    }
    mainWindow = null;
  });

  const scheduleSave = () => {
    if (boundsSaveTimer) clearTimeout(boundsSaveTimer);
    boundsSaveTimer = setTimeout(saveWindowState, 500);
  };

  mainWindow.on("resize", scheduleSave);
  mainWindow.on("move", scheduleSave);

  // レンダラに「ランチャーフォルダのパス」を通知
  mainWindow.webContents.on("did-finish-load", () => {
    if (isWindowAlive()) {
      mainWindow.webContents.send("launchers:path", launchersDir);
    }
  });
}

// ==============================
// IPC: 開始 / 停止
// ==============================

// チャット開始
ipcMain.on("chat:start", (event, payload) => {
  if (chatRequested) return;
  chatRequested = true;
  const currentChatGeneration = ++chatGeneration;
  requestSpeechWithChat();
  const translation = loadTranslationSettings();
  if (translation.enabled) startTranslation(translation.device);
  const inputStr = typeof payload === "object" && payload !== null
    ? String(payload.input || "")
    : String(payload || "");
  const obsSettings = typeof payload === "object" && payload !== null
    ? saveObsSettings(payload.obs || {})
    : loadObsSettings();
  console.log("IPC chat:start", inputStr);

  reportChatStatus('starting');

  refreshObsBrowserSources(obsSettings)
    .then((result) => {
      if (!isWindowAlive() || result.skipped) return;
      const count = Array.isArray(result.refreshed) ? result.refreshed.length : 0;
      const failed = Array.isArray(result.failed) ? result.failed.length : 0;
      mainWindow.webContents.send(
        "obs:status",
        failed > 0
          ? `OBS再読み込み: ${count}件成功 / ${failed}件失敗`
          : `OBS再読み込み: ${count}件完了`
      );
    })
    .catch((e) => {
      console.warn("OBS refresh error:", e?.message || e);
      if (isWindowAlive()) {
        mainWindow.webContents.send("obs:status", "OBS再読み込みエラー: " + (e?.message || String(e)));
      }
    });

  startLiveChat(inputStr,status=>{
    if(chatRequested&&currentChatGeneration===chatGeneration)reportChatStatus(status);
  }).catch((e) => {
    if (currentChatGeneration !== chatGeneration) return;
    chatRequested = false;
    stopSpeechWithChat();
    commentTranslation.stop();
    console.error("startLiveChat error:", e);
    reportChatStatus('stopped');
    if (isWindowAlive()) {
      mainWindow.webContents.send(
        "chat:error",
        e?.message || String(e)
      );
    }
  });

  // 同時接続数のウォッチも並行で開始
  resolveVideoId(inputStr)
    .then((videoId) => {
      if (!chatRequested || currentChatGeneration !== chatGeneration) return;
      if (videoId) {
        startConcurrentWatcher(videoId);
      } else {
        console.warn("ライブ配信が見つからず、同接ウォッチを開始できません");
      }
    })
    .catch((e) => {
      console.warn("同接ウォッチ用 videoId 解決エラー:", e?.message || e);
    });
});

ipcMain.handle("obs:get-settings", () => {
  return loadObsSettings();
});

ipcMain.handle("obs:save-settings", (_event, settings) => {
  return saveObsSettings(settings || {});
});

ipcMain.handle("obs:test", async (_event, settings) => {
  const saved = saveObsSettings(settings || {});
  try {
    const result = await refreshObsBrowserSources(saved, { force: true });
    return { ok: true, ...result };
  } catch (err) {
    return { ok: false, error: err?.message || String(err) };
  }
});

ipcMain.handle("obs:add-sources", async (_event, settings) => {
  const saved = saveObsSettings(settings || {});
  try {
    return await addObsBrowserSources(saved);
  } catch (err) {
    return { ok: false, error: err?.message || String(err) };
  }
});

// チャット停止
ipcMain.on("chat:stop", () => {
  chatGeneration++;
  chatRequested = false;
  stopSpeechWithChat();
  commentTranslation?.stop();
  console.log("IPC chat:stop");
  stopLiveChat();
  stopConcurrentWatcher();

  reportChatStatus('stopped');
});

// ランチャーフォルダを開くボタン用
ipcMain.on("launchers:open", () => {
  const dir = path.join(app.getPath("userData"), "obs-launchers");
  shell.openPath(dir);
});

// カラー設定の更新
ipcMain.on("colors:update", (_event, settings) => {
  if (settings.overlay) {
    currentColors.overlay = settings.overlay;
  }
  if (settings.supers) {
    currentColors.supers = settings.supers;
  }
  if (settings.nico) {
    currentColors.nico = settings.nico;
  }
  console.log("Settings updated:", currentColors);
});

// ==============================
// アプリライフサイクル
// ==============================

app.whenReady().then(() => {
  const chatDbPath = initChatStore(app.getPath("userData"));
  console.log("SQLite chat DB:", chatDbPath);
  loadParticipantState();
  loadOhayoState();
  prepareOptionalRuntime();
  prepareCommentTranslation();
  prepareSpeech();

  createOverlayServer();

  // 起動時にランチャーHTMLを生成
  const launchersDir = createObsLauncherFiles();

  // 設定画面を表示
  createMainWindow(launchersDir);

  // 環境変数で動画IDが指定されていれば、同接ウォッチを開始
  if (concurrentVideoId) {
    startConcurrentWatcher(concurrentVideoId);
  }
});

app.on("before-quit", () => {
  shuttingDown=true;
  optionalRuntime?.close();
  speech?.stop();
  commentTranslation?.stop();
  stopLiveChat();
  stopConcurrentWatcher();
  saveOhayoState();
  closeChatStore();
});

app.on("window-all-closed", () => {
  shuttingDown=true;
  optionalRuntime?.close();
  speech?.stop();
  commentTranslation?.stop();
  stopLiveChat();
  stopConcurrentWatcher();
  saveOhayoState();
  closeChatStore();
  if (process.platform !== "darwin") {
    app.quit();
  }
});

app.on("activate", () => {
  if (mainWindow === null) {
    const launchersDir = path.join(app.getPath("userData"), "obs-launchers");
    createMainWindow(launchersDir);
  }
});
