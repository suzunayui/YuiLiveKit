const {_electron:electron}=require('playwright');
const assert=require('node:assert/strict');
const path=require('node:path');
const fs=require('node:fs');
const root=path.resolve(__dirname,'..');
(async()=>{
  const desktop=await electron.launch({executablePath:path.join(root,'dist/win-unpacked/YuiLiveKit.exe'),
    env:{...process.env,YT_OVERLAY_TEST_PORT:'15007',YT_OVERLAY_TEST_USER_DATA:path.join(root,'test-output/niconico-render-profile')}});
  try{
    const windowEvent=desktop.waitForEvent('window');
    await desktop.evaluate(({BrowserWindow})=>{
      global.nicoTestWindow=new BrowserWindow({show:false,width:1280,height:720,webPreferences:{offscreen:true,backgroundThrottling:false}});
      nicoTestWindow.loadURL('about:blank');
    });
    const page=await windowEvent;
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    const settings={fontSize:64,fontFamily:'Meiryo',shadowEnabled:true,colorShadow:'#000000',alphaShadow:100,scrollDuration:8};
    await page.route('**/settings/nico',route=>route.fulfill({json:settings}));
    await page.route('**/comments?*',route=>route.fulfill({json:[]}));
    await page.route('https://fonts.**',route=>route.abort());
    await page.goto('http://127.0.0.1:15007/niconico');
    await page.evaluate(async()=>{await fetchAndApplySettings();spawnScrollingBullet({text:'おはようございます Good morning'});});
    const bullet=page.locator('.bullet');await bullet.waitFor();
    const first=await bullet.evaluate(el=>({x:el.getBoundingClientRect().x,stroke:getComputedStyle(el).webkitTextStrokeWidth,paint:getComputedStyle(el).paintOrder,shadow:getComputedStyle(el).textShadow}));
    assert.equal(first.stroke,'10px');assert.equal(first.paint,'stroke');assert.equal(first.shadow,'none');
    await page.waitForTimeout(200);
    const second=await bullet.evaluate(el=>({x:el.getBoundingClientRect().x,left:el.style.left,transform:el.style.transform}));
    assert.ok(second.x<first.x,'comments move to the left');assert.equal(second.left,'');assert.match(second.transform,/translate3d/);
    settings.shadowEnabled=false;await page.evaluate(()=>fetchAndApplySettings());
    assert.equal(await bullet.evaluate(el=>getComputedStyle(el).webkitTextStrokeWidth),'0px');
    settings.shadowEnabled=true;settings.alphaShadow=50;await page.evaluate(()=>fetchAndApplySettings());
    assert.equal(await bullet.evaluate(el=>getComputedStyle(el).webkitTextStrokeColor),'rgba(0, 0, 0, 0.5)');
    settings.alphaShadow=100;await page.evaluate(()=>fetchAndApplySettings());
    await page.evaluate(()=>{
      window.requestAnimationFrame=()=>0;
      document.body.style.background='#52778c';
      const original=document.querySelector('.bullet');const el=original.cloneNode(true);original.replaceWith(el);
      el.style.transform='translate3d(50px, 0, 0)';el.style.top='100px';
    });
    await page.waitForTimeout(100);
    const output=path.join(root,'test-output/niconico-smooth.png');
    await desktop.evaluate(async(_electron,output)=>{
      const image=await nicoTestWindow.webContents.capturePage();
      process.getBuiltinModule('fs').writeFileSync(output,image.toPNG());
    },output);
    assert.deepEqual(errors,[]);
    assert.ok(fs.statSync(output).size>1000);
    console.log('PASS: continuous text outlines, preserved color/opacity/off setting, transform scrolling and no renderer errors.');
  }finally{await desktop.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
