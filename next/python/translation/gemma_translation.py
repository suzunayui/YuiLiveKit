"""TranslateGemma text translation using a private, local llama.cpp server."""
import atexit
import json
import os
import re
import secrets
import socket
import subprocess
import time
import urllib.request
from pathlib import Path
from known_names import name_translation, name_glossary

REPOSITORY = 'bullerwins/translategemma-12b-it-GGUF'
REVISION = 'd7d1d8cc4ff53d4bc883ef33eae3894f07833b63'
FILENAME = 'translategemma-12b-it-Q4_K_M.gguf'
SHA256 = '9196d728812afbf5efc10b539298585725edc3a4ecc092c22fdde5bbaf41879e'
LANGUAGE_NAMES = dict(item.split(':', 1) for item in (
    'ar:Arabic bg:Bulgarian bn:Bengali ca:Catalan cs:Czech da:Danish de:German '
    'el:Greek en:English es:Spanish et:Estonian fa:Persian fi:Finnish fr:French '
    'gu:Gujarati he:Hebrew hi:Hindi hr:Croatian hu:Hungarian id:Indonesian '
    'is:Icelandic it:Italian ja:Japanese kn:Kannada ko:Korean lt:Lithuanian '
    'lv:Latvian ml:Malayalam mr:Marathi ms:Malay no:Norwegian nl:Dutch '
    'pl:Polish pt:Portuguese ro:Romanian ru:Russian sk:Slovak sl:Slovenian '
    'sr:Serbian sv:Swedish sw:Swahili ta:Tamil te:Telugu th:Thai tr:Turkish '
    'uk:Ukrainian ur:Urdu vi:Vietnamese zh:Chinese fil:Filipino'
).split())

def language_detector():
    from lingua import Language, LanguageDetectorBuilder
    # The detector also knows languages that TranslateGemma cannot translate.
    # Those can incorrectly win on short chat messages (e.g. "i like sushi"
    # was classified as Zulu), preventing a completion request altogether.
    languages = [language for language in Language.all()
                 if language.iso_code_639_1.name.lower() in LANGUAGE_NAMES]
    return LanguageDetectorBuilder.from_languages(*languages).build()

def translation_prompt(text, language):
    name = LANGUAGE_NAMES.get(language)
    if not name:
        raise ValueError(f'未対応の言語です: {language}')
    code = 'zh-Hans' if language == 'zh' else language
    # Treat chat control tokens occurring in a comment as literal text.
    text = re.sub(r'<(?:\|[^>]*\||/?(?:bos|eos|start_of_turn|end_of_turn))>',
                  lambda m: m[0].replace('<', '＜').replace('>', '＞'), text)
    instruction = (
        f'You are a professional {name} ({code}) to Japanese (ja) translator. '
        f'Your goal is to accurately convey the meaning and nuances of the original {name} text '
        'while adhering to Japanese grammar, vocabulary, and cultural sensitivities.\n'
        'Produce only the Japanese translation, without any additional explanations or commentary. '
        + name_glossary(text) +
        f'Please translate the following {name} text into Japanese:\n\n\n{text}'
    )
    return f'<bos><start_of_turn>user\n{instruction}<end_of_turn>\n<start_of_turn>model\n'

class TextTranslator:
    def __init__(self, cache, device='cpu', status=lambda text: None, offline=False):
        from huggingface_hub import hf_hub_download
        from langid.langid import LanguageIdentifier, model
        from tqdm.auto import tqdm
        self.server = None
        self.identifier = LanguageIdentifier.from_modelstring(model, norm_probs=True)
        self.identifier.set_languages([code for code in self.identifier.nb_classes if code in LANGUAGE_NAMES])
        self.detector = language_detector()
        class Progress(tqdm):
            def display(progress, *args, **kwargs):
                if progress.total and progress.total > 1000000:
                    status(f'TranslateGemma 12Bをダウンロード中 {100 * progress.n / progress.total:.0f}%（約7.3GB）')
        status('TranslateGemma 12Bを準備中（初回は約7.3GBをダウンロードします）')
        filename = None
        for existing in [cache, os.environ.get('SUBTITLE_SHARED_MODEL_DIR')]:
            if not existing:
                continue
            try:
                filename = hf_hub_download(REPOSITORY, FILENAME, revision=REVISION,
                    cache_dir=str(existing), local_files_only=True)
                break
            except OSError:
                pass
        if filename is None:
            filename = hf_hub_download(REPOSITORY, FILENAME, revision=REVISION,
                cache_dir=str(cache), local_files_only=offline, tqdm_class=Progress)
        executable = Path(os.environ.get('SUBTITLE_TRANSLATION_RUNTIME',
            str(Path(__file__).resolve().parent.parent / 'build' / 'translation-runtime' / 'llama-server.exe')))
        if not executable.is_file():
            raise ValueError('翻訳ランタイムが見つかりません。アプリを再インストールしてください。')
        # Match CUDA UI indices by GPU UUID/name rather than assuming Vulkan order.
        target = 'none'
        if device.startswith('cuda:'):
            gpu_index = int(device.split(':')[1])
            available = subprocess.run([str(executable), '--list-devices'], capture_output=True,
                encoding='utf-8', errors='replace', creationflags=subprocess.CREATE_NO_WINDOW, timeout=30)
            import ctypes
            cuda = ctypes.WinDLL('nvcuda.dll')
            cuda.cuInit(0)
            gpu = ctypes.c_int()
            if cuda.cuDeviceGet(ctypes.byref(gpu), gpu_index) != 0:
                raise ValueError('指定したGPUが見つかりません。')
            name = ctypes.create_string_buffer(256)
            cuda.cuDeviceGetName(name, 256, gpu)
            gpu_name = name.value.decode()
            matches = re.findall(r'^\s*(Vulkan\d+): (.*?) \(', available.stdout + available.stderr, re.M)
            matching = [key for key, value in matches if value == gpu_name]
            if len(matching) != 1:
                raise ValueError('翻訳用GPUを特定できません。CPUを選択してください。')
            target = matching[0]
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        self.url = f'http://127.0.0.1:{port}'
        self.key = secrets.token_hex(24)
        # Explicitly bypass any proxy settings for local inference.
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        status(f'TranslateGemma 12Bを読み込み中（{device}）')
        self.server = subprocess.Popen([str(executable), '-m', filename, '--host', '127.0.0.1',
            '--port', str(port), '--api-key', self.key, '-c', '4096', '-np', '1',
            '-t', '8', '-ngl', '0' if target == 'none' else '99', '--device', target,
            '--no-cache-prompt', '--no-jinja', '--chat-template', 'gemma'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        atexit.register(self.close)
        try:
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                if self.server.poll() is not None:
                    raise ValueError('モデルを読み込めませんでした。GPUメモリを空けるかCPUを選択してください。')
                try:
                    if self.request('/health').get('status') == 'ok':
                        return
                except (OSError, ValueError):
                    pass
                time.sleep(0.25)
            raise ValueError('翻訳モデルの読み込みがタイムアウトしました。')
        except Exception:
            self.close()
            raise

    def request(self, route, payload=None):
        body = json.dumps(payload, ensure_ascii=False).encode('utf-8') if payload is not None else None
        request = urllib.request.Request(self.url + route, data=body,
            headers={'Authorization':f'Bearer {self.key}', 'Content-Type':'application/json'})
        with self.http.open(request, timeout=120 if payload else 2) as response:
            return json.load(response)

    def detect(self, text):
        from text_translation import fixed_translation
        fixed = fixed_translation(text)
        if fixed:
            return fixed['language']
        detection_text = re.sub(r'https?://\S+', '', text)
        detected = self.detector.detect_language_of(detection_text)
        return detected.iso_code_639_1.name.lower() if detected else self.identifier.classify(detection_text)[0]

    def translate(self, text, language=None):
        text = str(text).strip()[:2000]
        named = name_translation(text)
        if named:
            return named
        language = language or self.detect(text)
        if language == 'ja':
            return {'ja':'', 'language':'ja'}
        result = self.request('/completion', {'prompt':translation_prompt(text, language),
            'n_predict':768, 'temperature':0, 'top_k':64, 'top_p':0.95,
            'stop':['<end_of_turn>', '<eos>'], 'cache_prompt':False})
        if result.get('stopped_limit') or result.get('truncated'):
            raise ValueError('コメントが長すぎるため翻訳を省略しました。')
        ja = result.get('content', '').strip()
        if not ja:
            raise ValueError('翻訳結果が空でした。')
        return {'ja':ja, 'language':language}

    def close(self):
        if self.server and self.server.poll() is None:
            self.server.terminate()
            try:
                self.server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.server.kill()

