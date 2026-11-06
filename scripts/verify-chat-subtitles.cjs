const {_electron:electron}=require('playwright');
const assert=require('node:assert/strict');
const path=require('node:path'),fs=require('node:fs');
const root=path.resolve(__dirname,'..');
(async()=>{
  const profile=path.join(root,'test-output/chat-subtitles-profile-'+Date.now());fs.mkdirSync(profile,{recursive:true});
  fs.writeFileSync(path.join(profile,'speech-settings.json'),JSON.stringify({config:{microphone:'',device:'cpu',model:'large-v3',threshold:'0.5'}}));
  const desktop=await electron.launch({executablePath:path.join(root,'dist/win-unpacked/YuiLiveKit.exe'),env:{...process.env,
    YT_OVERLAY_TEST_PORT:'15008',YT_OVERLAY_TEST_USER_DATA:profile,YT_OVERLAY_TEST_RUNTIME:path.join(root,'test-output/addon-runtime'),HF_HUB_OFFLINE:'1'}});
  try{
    const page=await desktop.firstWindow();const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await desktop.evaluate(()=>{const original=global.fetch;global.fetch=(url,...args)=>String(url).includes('youtube.com')?new Promise(()=>{}):original(url,...args);});
    await page.waitForFunction(()=>document.querySelector('#speechMicrophone').options.length>0,null,{timeout:30000});
    await page.locator('#mainTabs [data-tab="addons"]').click();await page.locator('#translationEnabled').uncheck();
    await page.locator('#inputTarget').fill('@LofiGirl');await page.locator('#btnStart').click();
    await page.waitForFunction(()=>document.querySelector('#speechStatus').textContent.includes('マイクを選択'));
    assert.equal(await page.evaluate(()=>require('electron').ipcRenderer.invoke('speech:get-state').then(s=>s.running)),false,'unconfigured subtitles remain stopped');
    await page.locator('#btnStop').click();
    await page.locator('#mainTabs [data-tab="speech"]').click();
    const mic=await page.locator('#speechMicrophone option').evaluateAll(options=>(options.find(o=>/HyperX/.test(o.textContent))||options[0]).value);
    await page.locator('#speechMicrophone').selectOption(mic);await page.locator('#speechDevice').selectOption('cuda:1');
    await page.waitForFunction(()=>require('electron').ipcRenderer.invoke('speech:get-state').then(s=>s.settings.config.device==='cuda:1'));
    const saved=JSON.parse(fs.readFileSync(path.join(profile,'speech-settings.json'),'utf8'));
    assert.equal(saved.config.microphone,mic);assert.equal(saved.config.device,'cuda:1','configuration saves without pressing subtitle start');
    await page.locator('#btnStart').click();
    await page.waitForFunction(()=>document.querySelector('#speechStatus').textContent.includes('字幕を生成中'),null,{timeout:120000});
    assert.equal(await page.locator('#speechStop').isEnabled(),true,'chat start launches real microphone subtitles');
    await page.locator('#btnStop').click();
    await page.waitForFunction(()=>!document.querySelector('#speechStart').disabled,null,{timeout:30000});
    assert.equal(await page.locator('#speechStop').isDisabled(),true,'chat stop stops the linked subtitles');
    // Start chat while device enumeration is pending after a reload.
    await page.locator('#speechReload').click();await page.locator('#btnStart').click();
    await page.waitForFunction(()=>document.querySelector('#speechStatus').textContent.includes('字幕を生成中'),null,{timeout:120000});
    await page.locator('#btnStop').click();
    await page.waitForFunction(()=>!document.querySelector('#speechStart').disabled,null,{timeout:30000});
    await page.locator('#speechStart').click();
    await page.waitForFunction(()=>document.querySelector('#speechStatus').textContent.includes('字幕を生成中'),null,{timeout:120000});
    await page.locator('#btnStart').click();await page.locator('#btnStop').click();await page.waitForTimeout(300);
    assert.equal(await page.locator('#speechStop').isEnabled(),true,'independently started subtitles keep running');
    await page.locator('#speechStop').click();
    assert.deepEqual(errors,[]);
    console.log('PASS: unconfigured skip, automatically saved settings, chat-linked real GPU subtitles start/stop, pending device preparation and independently started subtitles.');
  }finally{await desktop.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
