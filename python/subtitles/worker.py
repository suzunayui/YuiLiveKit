"""Microphone capture and local Whisper recognition/translation."""
import json
import os
import queue
import subprocess
import sys
import threading
import time
import base64
from contextlib import contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FROZEN = getattr(sys, 'frozen', False)
MODEL_DIR = Path(os.getenv('SUBTITLE_MODEL_DIR', str(ROOT / 'models')))
CONFIG_DIR = Path(os.getenv('SUBTITLE_CONFIG_DIR', str(ROOT)))
for stream in (sys.stdin, sys.stdout, sys.stderr):
    if stream is not None and hasattr(stream, 'reconfigure'):
        stream.reconfigure(encoding='utf-8')
os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
# Make pip-installed CUDA runtime libraries visible without modifying the system PATH.
handles = []
for vendor in [Path(sys.prefix) / "Lib/site-packages/nvidia", Path(getattr(sys, '_MEIPASS', ROOT)) / 'nvidia']:
    if vendor.exists():
        for dll in vendor.glob("**/bin"):
            os.environ["PATH"] = str(dll) + os.pathsep + os.environ.get("PATH", "")
            if hasattr(os, "add_dll_directory"):
                handles.append(os.add_dll_directory(str(dll)))

import numpy as np
import sounddevice as sd
from faster_whisper import WhisperModel
from segmentation import SpeechSegmenter, LocalVoiceDetector

output_lock = threading.Lock()
stop_event = threading.Event()
model_cache = None
model_key = None


def emit(kind, **data):
    with output_lock:
        print(json.dumps({"type": kind, **data}, ensure_ascii=False), flush=True)


def log_exception():
    import traceback
    traceback.print_exc(file=sys.stderr)


def get_devices():
    devices = []
    for index, device in enumerate(sd.query_devices()):
        if device["max_input_channels"] > 0:
            host = sd.query_hostapis(device["hostapi"])["name"]
            # Prefer WASAPI on Windows, avoiding duplicates from other host APIs.
            if sys.platform == "win32" and "WASAPI" not in host:
                continue
            devices.append({"id": index, "name": device["name"], "host": host})
    gpus = []
    try:
        result = subprocess.run(["nvidia-smi", "--query-gpu=index,name", "--format=csv,noheader"], capture_output=True, text=True, timeout=5, creationflags=0x08000000 if sys.platform == "win32" else 0)
        for line in result.stdout.strip().splitlines():
            index, name = line.split(",", 1)
            gpus.append({"id": int(index), "name": name.strip()})
    except Exception:
        pass
    emit("devices", devices=devices, gpus=gpus)


def load_model(config):
    global model_cache, model_key
    size = config.get("model", "large-v3")
    if size not in {"base", "small", "medium", "large-v3"}:
        raise ValueError("未対応のモデルです")
    requested = config.get("device", "cpu")
    device = "cuda" if requested.startswith("cuda:") else "cpu"
    index = int(requested.split(":")[1]) if device == "cuda" else 0
    key = (size, device, index)
    if model_cache is not None and model_key == key:
        return model_cache
    model_cache = None
    model_key = None
    emit("status", text=f"{size}モデルを準備中（初回はダウンロードします）")
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    # Reuse a complete existing model without moving or modifying its cache.
    model_path = size
    shared = os.getenv('SUBTITLE_SHARED_MODEL_DIR')
    if shared:
        from faster_whisper.utils import download_model
        try:
            cached = Path(download_model(size, cache_dir=shared, local_files_only=True))
            if all((cached / file).is_file() for file in ('model.bin', 'config.json', 'tokenizer.json')):
                model_path = str(cached)
        except Exception:
            pass
    model = WhisperModel(model_path, device=device, device_index=index, compute_type="int8_float16" if device == "cuda" else "int8", cpu_threads=8, download_root=str(MODEL_DIR))
    # CUDA libraries may load lazily: check actual inference before capturing audio.
    segments, _ = model.transcribe(np.zeros(16000, dtype=np.float32), language="ja", beam_size=1)
    list(segments)
    model_cache, model_key = model, key
    return model


def convert_audio(audio, rate):
    if rate == 16000:
        return np.ascontiguousarray(audio, dtype=np.float32)
    # Microphone input is typically 48 kHz. Averaging provides simple anti-aliasing.
    if rate % 16000 == 0:
        factor = rate // 16000
        audio = audio[:len(audio) // factor * factor]
        return audio.reshape(-1, factor).mean(axis=1).astype(np.float32)
    positions = np.arange(int(len(audio) * 16000 / rate)) * rate / 16000
    return np.interp(positions, np.arange(len(audio)), audio).astype(np.float32)


def recognize(model, audio):
    options = dict(language="ja", beam_size=5, temperature=0.0,
                   condition_on_previous_text=False, vad_filter=True,
                   vad_parameters={"threshold": 0.35, "min_speech_duration_ms": 150,
                                   "min_silence_duration_ms": 700, "speech_pad_ms": 400})
    segments, _ = model.transcribe(audio, task="transcribe", **options)
    ja = "".join(segment.text for segment in segments
                 if segment.no_speech_prob < 0.8 and segment.avg_logprob >= -1.0).strip()
    if not ja:
        return None
    segments, _ = model.transcribe(audio, task="translate", **options)
    en = " ".join(segment.text.strip() for segment in segments).strip()
    return ja, en


def inference_process():
    """Keep CUDA model execution on its own process's main thread."""
    try:
        config = json.loads(sys.stdin.readline())
        model = load_model(config)
        emit("ready")
        for line in sys.stdin:
            request = json.loads(line)
            audio = np.frombuffer(base64.b64decode(request['audio']), dtype=np.float32).copy()
            emit("result", value=recognize(model, audio))
    except Exception as error:
        log_exception()
        emit("error", text=str(error))


def read_inference(process):
    line = process.stdout.readline()
    if not line:
        raise RuntimeError(f'認識プロセスが終了しました ({process.poll()})')
    response = json.loads(line)
    if response['type'] == 'error':
        raise RuntimeError(response['text'])
    return response


@contextmanager
def capture_stream(**kwargs):
    stream = sd.InputStream(**kwargs)
    try:
        stream.start()
        yield stream
    finally:
        # Input streams need no playback drain; abort also handles immediate stop.
        try:
            stream.abort()
        finally:
            stream.close()


def run_session(config):
    errors = queue.Queue()
    consumer = None
    inference = None
    pending = queue.Queue(maxsize=2)
    try:
        inference_args = [sys.executable, '--inference'] if FROZEN else [sys.executable, '-u', str(Path(__file__).resolve()), '--inference']
        inference = subprocess.Popen(inference_args,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     text=True, encoding='utf-8',
                                     creationflags=0x08000000 if sys.platform == 'win32' else 0)
        # GUI-launched workers must not rely on an inherited console error handle.
        # Forward the child log separately so it cannot block recognition output.
        def forward_errors():
            for line in inference.stderr:
                sys.stderr.write(line)
                sys.stderr.flush()
        threading.Thread(target=forward_errors, daemon=True).start()
        inference.stdin.write(json.dumps(config) + '\n')
        inference.stdin.flush()
        while not stop_event.is_set():
            preparation = read_inference(inference)
            if preparation['type'] == 'ready':
                break
            if preparation['type'] == 'status':
                emit('status', text=preparation['text'])
        if stop_event.is_set():
            return
        microphone = int(config["microphone"])
        info = sd.query_devices(microphone, "input")
        rate = int(info["default_samplerate"])
        blocksize = max(1, int(rate * 0.03))
        threshold = max(0.3, min(0.8, float(config.get("threshold", 0.5))))
        segmenter = SpeechSegmenter(threshold)
        detector = LocalVoiceDetector()
        def process():
            while not stop_event.is_set():
                try:
                    audio = pending.get(timeout=0.2)
                except queue.Empty:
                    continue
                try:
                    started = time.monotonic()
                    inference.stdin.write(json.dumps({'audio': base64.b64encode(audio.tobytes()).decode('ascii')}) + '\n')
                    inference.stdin.flush()
                    result = None
                    while not stop_event.is_set():
                        response = read_inference(inference)
                        if response['type'] == 'result':
                            result = response['value']
                            break
                    if result and not stop_event.is_set():
                        emit("subtitle", ja=result[0], en=result[1], latency=round(time.monotonic() - started, 2))
                except Exception as error:
                    log_exception()
                    errors.put(error)
                    stop_event.set()
                    return

        consumer = threading.Thread(target=process, daemon=True)
        consumer.start()
        last_level = 0
        channels = min(2, int(info['max_input_channels']))
        with capture_stream(device=microphone, channels=channels, samplerate=rate, blocksize=blocksize, dtype="int16") as stream:
            emit("running", value=True)
            emit("status", text="字幕を生成中 · 日本語で話してください")
            while not stop_event.is_set():
                data, overflowed = stream.read(blocksize)
                # Request native input channels and PCM rather than asking WASAPI
                # to mix a stereo endpoint down to a mono float stream.
                chunk = data[:, 0].astype(np.float32) / 32768
                audio_frame = convert_audio(chunk, rate)
                probability = detector.push(audio_frame)
                rms = float(np.sqrt(np.mean(chunk ** 2)))
                if time.monotonic() - last_level > 0.15:
                    emit("level", value=min(1.0, rms * 8))
                    last_level = time.monotonic()
                audio = segmenter.push(audio_frame, probability)
                if audio is not None:
                    try:
                        pending.put_nowait(audio)
                    except queue.Full:
                        # Keep recent speech rather than accumulating delayed subtitles.
                        try:
                            pending.get_nowait()
                        except queue.Empty:
                            pass
                        pending.put_nowait(audio)
                        emit("status", text="処理が追いついていません。小さいモデルに切り替えてください")
    except Exception as error:
        log_exception()
        errors.put(error)
    finally:
        stop_event.set()
        if consumer:
            consumer.join()
        if inference:
            if inference.poll() is None:
                inference.terminate()
            inference.wait(timeout=5)
        if not errors.empty():
            error = str(errors.get())
            emit("error", text=f"音声処理エラー: {error}")
        else:
            emit("status", text="停止しました")
        emit("level", value=0)


def main():
    get_devices()
    commands = queue.Queue()

    def receive_commands():
        for line in sys.stdin:
            try:
                command = json.loads(line)
                if command.get("action") == "start":
                    stop_event.clear()
                    commands.put(command)
                elif command.get("action") == "stop":
                    stop_event.set()
            except Exception as error:
                emit("error", text=str(error))
        stop_event.set()
        commands.put(None)

    threading.Thread(target=receive_commands, daemon=True).start()
    # PortAudio/WASAPI must open and close streams on its initializing thread.
    # Read commands separately so stop remains responsive during inference.
    while True:
        command = commands.get()
        if command is None:
            break
        run_session(command)
        emit("running", value=False)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--inference":
        inference_process()
    elif len(sys.argv) > 1 and sys.argv[1] == "--check":
        get_devices()
    else:
        main()
