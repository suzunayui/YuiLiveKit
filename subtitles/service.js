const {spawn, spawnSync} = require('node:child_process');
const {EventEmitter} = require('node:events');
const {normalizeStyle} = require('./style');
const {displaySubtitle} = require('./profanity');

class SpeechSubtitles extends EventEmitter {
  constructor(command, args, options, settings) {
    super(); Object.assign(this, {command,args,options});
    this.state = {running:false, ready:false, status:'マイクを準備中', devices:[], gpus:[], level:0, subtitle:null, settings, style:normalizeStyle(settings.style)};
    this.child = null;
    this.rawSubtitle = null;
  }
  publish() {this.emit('state',this.state);}
  prepare() {
    if (this.child) return;
    let child;
    try {child = spawn(this.command,this.args,this.options);}
    catch (error) {
      // A damaged executable throws here instead of emitting 'error'.
      Object.assign(this.state,{ready:false,status:'音声処理を起動できません: '+error.message}); this.publish();
      return;
    }
    this.child = child;
    let buffer = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data',chunk=>{
      buffer += chunk;
      for (let end; (end=buffer.indexOf('\n'))>=0;) {
        const line = buffer.slice(0,end); buffer=buffer.slice(end+1);
        if (this.child !== child) continue;
        let event; try {event=JSON.parse(line);} catch {continue;}
        if (event.type==='devices') Object.assign(this.state,{devices:event.devices,gpus:event.gpus,ready:true,status:'停止中'});
        if (event.type==='status') this.state.status=event.text;
        if (event.type==='running') this.state.running=event.value;
        if (event.type==='level') this.state.level=event.value;
        if (event.type==='subtitle' && this.state.running) this.acceptSubtitle(event.ja,event.en);
        if (event.type==='error') this.state.status=event.text;
        this.publish();
      }
    });
    // Drain diagnostics without retaining microphone content on disk.
    child.stderr.resume();
    child.stdin.on('error',()=>{});
    const failed = message=>{
      if (this.child!==child) return;
      this.child=null;this.rawSubtitle=null; Object.assign(this.state,{running:false,ready:false,level:0,subtitle:null,status:message}); this.publish();
    };
    child.on('error',error=>failed('音声処理を起動できません: '+error.message));
    child.on('exit',code=>failed(`音声処理が終了しました (${code})。デバイスを再読み込みしてください`));
  }
  start(settings) {
    if (this.state.running || !this.state.ready) return false;
    this.state.settings=settings; this.state.style=normalizeStyle(settings.style);
    this.rawSubtitle=null;
    Object.assign(this.state,{running:true,subtitle:null,status:'音声認識モデルを準備中'}); this.publish();
    this.child.stdin.write(JSON.stringify({action:'start',...settings.config})+'\n'); return true;
  }
  stop(reload=false) {
    this.rawSubtitle=null;
    const child=this.child; this.child=null;
    if (child?.pid) {
      if (process.platform==='win32') spawnSync('taskkill',['/PID',String(child.pid),'/T','/F'],{windowsHide:true,timeout:5000,stdio:'ignore'});
      else child.kill();
    }
    Object.assign(this.state,{running:false,ready:false,subtitle:null,level:0,status:'停止中'}); this.publish();
    if (reload) this.prepare();
  }
  acceptSubtitle(ja,en) {
    this.rawSubtitle={ja,en,receivedAt:Date.now()};
    this.state.subtitle=displaySubtitle(this.rawSubtitle,this.state.style.maskProfanity);
  }
  update(settings) {
    this.state.settings=settings;this.state.style=normalizeStyle(settings.style);
    if(this.rawSubtitle)this.state.subtitle=displaySubtitle(this.rawSubtitle,this.state.style.maskProfanity);
    this.publish();
  }
  clear() {this.rawSubtitle=null;this.state.subtitle=null;this.publish();}
}
module.exports={SpeechSubtitles};
