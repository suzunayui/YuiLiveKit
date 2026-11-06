const {_electron:electron}=require('playwright');
const assert=require('node:assert/strict'),path=require('node:path');
const root=path.resolve(__dirname,'..');
(async()=>{
  const desktop=await electron.launch({executablePath:path.join(root,'dist/win-unpacked/YuiLiveKit.exe'),env:{...process.env,
    YT_OVERLAY_TEST_PORT:'15011',YT_OVERLAY_TEST_USER_DATA:path.join(root,'test-output/supers-profile-'+Date.now())}});
  try{
    const page=await desktop.firstWindow();const errors=[];page.on('pageerror',e=>errors.push(e.message));
    const timestamp=Date.now();
    const supported=[
      {id:'paid',kind:'paid',text:'Thank you!',amount:1000,amount_text:'￥1,000',colors:{body_bg:'#ff8800',body_text:'#ffffff'}},
      {id:'sticker',kind:'sticker',text:'Super sticker'},
      {id:'join',kind:'membership',text:'New member'},
      {id:'milestone',kind:'membership',text:'Member for 12 months Always thanks <img src=x onerror=alert(1)>',parts:[{type:'text',text:'Always thanks <img src=x onerror=alert(1)>'}]},
      {id:'gift',kind:'gift_purchase',text:'Gift purchase'},
      {id:'redeem',kind:'gift_redeem',text:'Gift received'}
    ].map((msg,index)=>({...msg,author:'Support '+index,video_id:'video-a',timestamp_ms:timestamp+index}));
    await desktop.evaluate(({app},{supported,timestamp})=>{
      const store=process.getBuiltinModule('module').createRequire(app.getAppPath()+'/package.json')('./chatStore');
      store.saveComment({id:'old-stream',kind:'paid',text:'Another stream',author:'Old',video_id:'video-old',timestamp_ms:timestamp-10000});
      for(const msg of supported)store.saveComment(msg);
      for(let i=0;i<600;i++)store.saveComment({id:'normal-'+i,kind:'text',text:'Normal '+i,author:'Viewer',is_member:true,video_id:'video-a',timestamp_ms:timestamp+100+i});
    },{supported,timestamp});
    await page.locator('#mainTabs [data-tab="supers"]').click();
    await page.waitForFunction(()=>document.querySelectorAll('#superList .list-item').length===6);
    assert.equal(await page.locator('#viewerSupersCard').isVisible(),true);
    assert.equal(await page.locator('#viewerCommentsCard').isHidden(),true);
    assert.equal(await page.locator('#superUnreadOnly').isChecked(),false);
    assert.deepEqual(await page.locator('#superList .list-item').evaluateAll(rows=>rows.map(row=>row.dataset.id)),supported.map(msg=>msg.id));
    assert.equal(await page.locator('#superList img').count(),0,'milestone text is rendered safely');
    assert.equal(await page.locator('#superList [data-id="milestone"] .membership-detail').textContent(),'Member for 12 months');
    const api=await (await fetch('http://127.0.0.1:15011/api/support-comments?video=video-a')).json();
    assert.equal(api.length,6);assert.equal(api[0].amount,1000);assert.equal(api[0].colors.body_bg,'#ff8800');
    assert.equal((await (await fetch('http://127.0.0.1:15011/comments?limit=200')).json()).length,200);
    await page.locator('#btnSuperCopy').click();await page.waitForFunction(()=>document.querySelector('#superCount').textContent.includes('コピー済み'));
    const copied=await desktop.evaluate(({clipboard})=>clipboard.readText());
    assert.match(copied,/Member for 12 months/);assert.doesNotMatch(copied,/Normal /);
    await page.locator('#superList [data-id="paid"]').click();
    await page.locator('#superUnreadOnly').check();assert.equal(await page.locator('#superList .list-item').count(),5);
    await page.locator('#superUnreadOnly').uncheck();assert.equal(await page.locator('#superList .list-item').count(),6);
    await page.reload();await page.waitForFunction(()=>document.querySelectorAll('#superList .list-item').length===6);
    assert.equal(await page.locator('#viewerSupersCard').isVisible(),true,'tab selection persists');
    assert.equal(await page.locator('#superList [data-id="paid"]').evaluate(el=>el.classList.contains('unread')),false,'read state persists');
    await page.locator('#btnSuperMarkAll').click();await page.locator('#superUnreadOnly').check();assert.equal(await page.locator('#superList .list-item').count(),0);
    await page.locator('#superUnreadOnly').uncheck();assert.equal(await page.locator('#superList .list-item').count(),6,'read comments remain available');
    await page.locator('#mainTabs [data-tab="comments"]').click();assert.equal(await page.locator('#viewerCommentsCard').isVisible(),true);
    await page.locator('#mainTabs [data-tab="supers"]').click();
    await desktop.evaluate(({app},timestamp)=>process.getBuiltinModule('module').createRequire(app.getAppPath()+'/package.json')('./chatStore').saveComment(
      {id:'new-stream',kind:'membership',text:'New stream milestone',author:'New',video_id:'video-b',timestamp_ms:timestamp+10000}),timestamp);
    await page.waitForFunction(()=>document.querySelectorAll('#superList .list-item').length===1&&document.querySelector('#superList .list-item').dataset.id==='new-stream');
    assert.equal((await (await fetch('http://127.0.0.1:15011/api/support-comments?video=video-a')).json()).length,6,'previous stream data is retained in SQLite');
    assert.deepEqual(errors,[]);
    console.log('PASS: dedicated tab, paid/sticker/membership milestone/gifts only, retention beyond 600 ordinary member comments, SQLite reload, amount/color, safe text, unread/copy, and stream isolation.');
  }finally{await desktop.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
