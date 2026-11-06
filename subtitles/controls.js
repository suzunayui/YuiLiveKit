const {ipcRenderer} = require('electron');
const $=id=>document.getElementById(id);
let state=null, deviceKey='', timer;
function options(select,items,value) {
  select.replaceChildren(...items.map(item=>{const option=document.createElement('option');option.value=item.value;option.textContent=item.label;return option;}));
  if(items.some(item=>item.value===value)) select.value=value;
}
function render(next) {
  state=next;
  const alignment=state.style.alignment||'center';
  for(const radio of $('speechAlignment').querySelectorAll('input')) radio.checked=radio.value===alignment;
  $('speechPreview').style.textAlign=alignment;
  const key=JSON.stringify([state.devices,state.gpus]);
  if(key!==deviceKey){
    deviceKey=key;
    options($('speechMicrophone'),state.devices.map(device=>({value:String(device.id),label:device.name})),state.settings.config.microphone);
    options($('speechDevice'),[{value:'cpu',label:'CPU（低速）'},...state.gpus.map(gpu=>({value:`cuda:${gpu.id}`,label:gpu.name}))],state.settings.config.device);
  }
  $('speechStatus').textContent=state.status;
  $('speechLevel').value=state.level;
  $('speechStart').disabled=!state.ready||state.running||!state.devices.length||($('speechDevice').value!=='cpu'&&!state.gpuReady);
  $('speechStop').disabled=!state.running;
  $('speechReload').disabled=state.running;
  for(const id of ['speechMicrophone','speechDevice','speechModel','speechThreshold']) $(id).disabled=state.running;
  clearTimeout(timer);
  const remaining=state.subtitle ? state.style.duration*1000-(Date.now()-state.subtitle.receivedAt) : 0;
  const clear=()=>{ $('speechJapanese').textContent='';$('speechEnglish').textContent='';};
  if(remaining>0){$('speechJapanese').textContent=state.subtitle.ja;$('speechEnglish').textContent=state.subtitle.en;timer=setTimeout(clear,remaining);}else clear();
}
function style() {return {...state.style,fontSize:$('speechFontSize').value,englishSize:$('speechEnglishSize').value,duration:$('speechDuration').value,maskProfanity:$('speechMaskProfanity').checked,alignment:$('speechAlignment').querySelector('input:checked')?.value||'center'};}
function settings() {return {config:{microphone:$('speechMicrophone').value,device:$('speechDevice').value,model:$('speechModel').value,threshold:$('speechThreshold').value},style:style(),startWithChat:$('speechStartWithChat').checked};}
async function invoke(channel,arg){try{render(await ipcRenderer.invoke(channel,arg));}catch(error){$('speechStatus').textContent=error.message;}}
$('speechStart').onclick=()=>invoke('speech:start',settings());
$('speechStop').onclick=()=>invoke('speech:stop');
$('speechReload').onclick=()=>invoke('speech:reload');
for(const id of ['speechMicrophone','speechDevice','speechModel','speechThreshold']) $(id).onchange=()=>{if(state)invoke('speech:settings',settings());};
$('speechStartWithChat').onchange=()=>invoke('speech:start-with-chat',$('speechStartWithChat').checked);
$('speechClear').onclick=()=>invoke('speech:clear');
for(const id of ['speechFontSize','speechEnglishSize','speechDuration','speechMaskProfanity']) $(id).onchange=()=>invoke('speech:style',style());
for(const radio of $('speechAlignment').querySelectorAll('input')) radio.onchange=()=>invoke('speech:style',style());
ipcRenderer.on('speech:state',(_event,next)=>render(next));
ipcRenderer.invoke('speech:get-state').then(next=>{
  const config=next.settings.config;
  $('speechModel').value=config.model;$('speechThreshold').value=config.threshold;
  $('speechFontSize').value=next.style.fontSize;$('speechEnglishSize').value=next.style.englishSize;$('speechDuration').value=next.style.duration;
  $('speechMaskProfanity').checked=next.style.maskProfanity;
  $('speechStartWithChat').checked=next.settings.startWithChat;
  render(next);
}).catch(error=>{$('speechStatus').textContent=error.message;});
