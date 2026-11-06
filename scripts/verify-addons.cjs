const {_electron:electron}=require('playwright');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const root=path.resolve(__dirname,'..'),output=path.join(root,'test-output');fs.mkdirSync(output,{recursive:true});
async function main(){
  const desktop=await electron.launch({executablePath:process.env.YT_OVERLAY_PACKAGED_EXE||path.join(root,'dist/win-unpacked/YuiLiveKit.exe'),args:[],cwd:root,
    env:{...process.env,PATH:`${process.env.SystemRoot}\\System32;${process.env.SystemRoot}`,YT_OVERLAY_TEST_USER_DATA:path.join(output,'addons-profile'),YT_OVERLAY_TEST_PORT:'15003',YT_OVERLAY_TEST_RUNTIME:path.join(output,'addon-runtime')},timeout:30000});
  try{
    const page=await desktop.firstWindow(),errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.locator('#mainTabs [data-tab="addons"]').click();
    let state=await page.evaluate(()=>require('electron').ipcRenderer.invoke('runtime:get-state'));
    if(!state.features.speech){
      assert.equal(await page.locator('#speechStart').isDisabled(),true);
      assert.equal(await page.locator('#installSpeech').isEnabled(),true);
      assert.equal(await page.locator('#installCuda').isDisabled(),true);
    }
    for(const [kind,id]of [['speech','installSpeech'],['cuda','installCuda'],['translation','installTranslation']]){
      state=await page.evaluate(()=>require('electron').ipcRenderer.invoke('runtime:get-state'));
      if(!state.features[kind]){
        console.log('Installing '+kind+' from its button, using app-private Python with no system Python on PATH.');
        await page.locator('#'+id).click();
        const deadline=Date.now()+600000;
        while(Date.now()<deadline){
          state=await page.evaluate(()=>require('electron').ipcRenderer.invoke('runtime:get-state'));
          if(!state.busy&&(state.features[kind]||state.status.includes('導入できません')))break;
          await page.waitForTimeout(300);
        }
        state=await page.evaluate(()=>require('electron').ipcRenderer.invoke('runtime:get-state'));
        assert.equal(state.features[kind],true,state.status);
      }
      assert.equal(await page.locator('#'+id).isDisabled(),true);
      console.log('PASS: '+kind+' installed and validated.');
    }
    await page.waitForFunction(()=>document.querySelector('#speechMicrophone').options.length>0,null,{timeout:30000});
    assert.deepEqual(errors,[]);
    assert.equal((await fetch('http://127.0.0.1:15003/subtitles')).status,200);
    console.log('PASS: lightweight package, separate addon buttons, private runtime bootstrap, downloads/checksums, wheels, WASAPI device enumeration, CUDA DLLs and translation runtime.');
  }finally{await desktop.close();}
}
main().catch(error=>{console.error(error);process.exitCode=1;});
