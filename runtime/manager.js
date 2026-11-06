const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const {spawn,spawnSync}=require('node:child_process');
const {EventEmitter}=require('node:events');
const PYTHON={url:'https://www.python.org/ftp/python/3.13.15/python-3.13.15-embed-amd64.zip',sha:'d1f04d990aee1253d8569e8e5104e30fa9f5fa830899f14843448872d936a2cf'};
const PIP={url:'https://files.pythonhosted.org/packages/f3/6e/1736e5b4ae2b778ef2f81c47d797de9f891d4d8acb047a24ca37a60294dd/pip-26.2.1-py3-none-any.whl',sha:'71138adf1f4ca900cdb7d289c21b7494329f2332b6d85f0e1c42108c0384ed3e'};
const LLAMA={url:'https://github.com/ggml-org/llama.cpp/releases/download/b11429/llama-b11429-bin-win-vulkan-x64.zip',sha:'1bfe78ad9168b79fa02bf67f6af9f5e17a966d824d77238517f7bef12ac73b36'};
const quote=value=>"'"+value.replaceAll("'","''")+"'";
function copyTree(source,target){
  fs.mkdirSync(target,{recursive:true});
  for(const item of fs.readdirSync(source,{withFileTypes:true})){
    if(item.name==='__pycache__'||item.name.endsWith('.pyc'))continue;
    const from=path.join(source,item.name),to=path.join(target,item.name);
    if(item.isDirectory())copyTree(from,to);else fs.writeFileSync(to,fs.readFileSync(from));
  }
}
class OptionalRuntime extends EventEmitter {
  constructor(root,source,packaged){
    super();Object.assign(this,{root,source,packaged});this.child=null;this.controller=null;
    this.python=packaged?path.join(root,'python','python.exe'):path.join(source,'.venv/Scripts/python.exe');
    this.backends=path.join(root,'backends');this.llama=packaged?path.join(root,'llama','llama-server.exe'):path.join(source,'build/translation-runtime/llama-server.exe');
    this.state={busy:false,installing:null,statusKind:null,status:'使いたい機能の「導入」ボタンを押してください',features:{}};
    // The search path holds absolute paths, so it must follow the runtime if the data folder moves.
    if(packaged&&fs.existsSync(path.join(path.dirname(this.python),'bootstrap.ready')))this.writePythonSearchPath();
    if(packaged)for(const kind of ['speech','translation'])if(this.ready(kind))copyTree(path.join(source,kind==='speech'?'subtitles':'translation','backend'),path.dirname(this.worker(kind)));
    this.refresh();
  }
  writePythonSearchPath(){
    fs.writeFileSync(path.join(path.dirname(this.python),'python313._pth'),['python313.zip','.','Lib/site-packages',path.join(this.backends,'subtitles'),path.join(this.backends,'translation'),'import site',''].join('\n'));
  }
  ready(kind){
    if(!this.packaged)return fs.existsSync(this.python)&&(kind!=='translation'||fs.existsSync(this.llama));
    try{
      if(!fs.existsSync(this.python)||JSON.parse(fs.readFileSync(path.join(this.root,kind+'.ready.json'),'utf8')).version!==1)return false;
      if(kind==='translation')return fs.existsSync(this.llama)&&fs.existsSync(this.worker(kind));
      if(kind==='speech')return fs.existsSync(this.worker(kind));
      const vendor=path.join(path.dirname(this.python),'Lib/site-packages/nvidia');
      return fs.existsSync(path.join(vendor,'cublas/bin/cublas64_12.dll'))&&fs.existsSync(path.join(vendor,'cudnn/bin/cudnn64_9.dll'));
    }catch{return false;}
  }
  refresh(){this.state.features=Object.fromEntries(['speech','translation','cuda'].map(kind=>[kind,this.ready(kind)]));this.emit('state',this.state);}
  status(text){this.state.status=text;this.refresh();}
  worker(kind){return this.packaged?path.join(this.backends,kind==='speech'?'subtitles':'translation','worker.py'):path.join(this.source,kind==='speech'?'subtitles':'translation','backend','worker.py');}
  async download(asset,name){
    const target=path.join(this.root,name);const response=await fetch(asset.url,{signal:this.controller.signal});
    if(!response.ok)throw Error(`ダウンロードに失敗しました (${response.status})`);
    const handle=await fs.promises.open(target,'w'),hash=crypto.createHash('sha256');let size=0,last=0;
    try{for await(const chunk of response.body){await handle.write(chunk);hash.update(chunk);size+=chunk.length;if(Date.now()-last>700){last=Date.now();this.status(`${name} を取得中: ${(size/1048576).toFixed(1)} MB`);}}}finally{await handle.close();}
    if(hash.digest('hex')!==asset.sha)throw Error('ダウンロードの検証に失敗しました。もう一度導入してください');return target;
  }
  run(command,args,report=false){
    return new Promise((resolve,reject)=>{
      const child=spawn(command,args,{windowsHide:true,cwd:this.root,env:{...process.env,PYTHONUTF8:'1',PIP_DISABLE_PIP_VERSION_CHECK:'1',PYTHONNOUSERSITE:'1'},stdio:['ignore','pipe','pipe']});this.child=child;
      let tail='',buffer='';const consume=chunk=>{const text=chunk.toString('utf8');tail=(tail+text).slice(-3000);if(report){buffer+=text;const lines=buffer.split(/[\r\n]+/);buffer=lines.pop();for(const line of lines)if(line.trim())this.status(line.trim().slice(0,240));}};
      child.stdout.on('data',consume);child.stderr.on('data',consume);
      child.on('error',reject);child.on('close',code=>{if(this.child===child)this.child=null;if(code===0)resolve();else reject(Error(`導入処理に失敗しました (${code}): ${tail.slice(-700)}`));});
    });
  }
  expand(zip,target){return this.run(path.join(process.env.SystemRoot,'System32/WindowsPowerShell/v1.0/powershell.exe'),['-NoProfile','-NonInteractive','-Command',`Expand-Archive -LiteralPath ${quote(zip)} -DestinationPath ${quote(target)} -Force`]);}
  async bootstrap(){
    const folder=path.dirname(this.python),marker=path.join(folder,'bootstrap.ready');
    if(fs.existsSync(marker))return;
    this.status('アプリ専用の実行環境を導入中');
    const zip=await this.download(PYTHON,'python.zip');await this.expand(zip,folder);
    const pip=await this.download(PIP,'pip.zip');await this.expand(pip,folder);
    fs.mkdirSync(path.join(folder,'Lib/site-packages'),{recursive:true});
    this.writePythonSearchPath();
    await this.run(this.python,['-m','pip','--version']);fs.writeFileSync(marker,'3.13.15 / pip 26.2.1\n');
    fs.unlinkSync(zip);fs.unlinkSync(pip);
  }
  async install(kind){
    if(!['speech','translation','cuda'].includes(kind))throw Error('未対応の機能です');
    if(this.state.busy||this.ready(kind))return;
    this.state.busy=true;this.state.installing=kind;this.state.statusKind=kind;this.state.status='導入を準備中';this.controller=new AbortController();this.refresh();
    try{
      fs.mkdirSync(this.root,{recursive:true});await this.bootstrap();
      const target=path.join(this.root,'requirements');fs.mkdirSync(target,{recursive:true});
      const backend=kind==='translation'?'translation':'subtitles';
      const requirements=kind==='cuda'?'requirements-cuda.txt':'requirements.txt';
      const file=path.join(target,kind+'.txt');fs.copyFileSync(path.join(this.source,backend,requirements),file);
      const vendor=path.join(this.root,'vendor');copyTree(path.join(this.source,'runtime/vendor'),vendor);
      this.status('必要なライブラリを導入中');
      await this.run(this.python,['-m','pip','install','--upgrade','--only-binary=:all:','--no-warn-script-location','--index-url','https://pypi.org/simple','--find-links',vendor,'--target',path.join(path.dirname(this.python),'Lib/site-packages'),'-r',file],true);
      if(kind==='translation'){
        const zip=await this.download(LLAMA,'llama.zip');await this.expand(zip,path.dirname(this.llama));fs.unlinkSync(zip);
        fs.copyFileSync(path.join(this.source,'runtime/LICENSE-llama.cpp.txt'),path.join(path.dirname(this.llama),'LICENSE-llama.cpp.txt'));
      }
      if(kind!=='cuda')copyTree(path.join(this.source,backend,'backend'),path.join(this.backends,backend));
      const check=kind==='speech'?'import sounddevice, faster_whisper, onnxruntime':kind==='translation'?'import langid, lingua, huggingface_hub':'from pathlib import Path;import sys;p=Path(sys.prefix)/"Lib/site-packages/nvidia";assert (p/"cublas/bin/cublas64_12.dll").is_file() and (p/"cudnn/bin/cudnn64_9.dll").is_file()';
      await this.run(this.python,['-c',check]);
      fs.writeFileSync(path.join(this.root,kind+'.ready.json'),JSON.stringify({version:1,installedAt:Date.now()}));
      this.status('導入が完了しました');this.emit('installed',kind);
    }catch(error){this.status(`導入できませんでした。再試行できます: ${error.message}`);}
    finally{this.state.busy=false;this.state.installing=null;this.controller=null;this.refresh();}
  }
  close(){this.controller?.abort();if(this.child?.pid){spawnSync('taskkill',['/PID',String(this.child.pid),'/T','/F'],{windowsHide:true,timeout:5000,stdio:'ignore'});this.child=null;}}
}
module.exports={OptionalRuntime};
