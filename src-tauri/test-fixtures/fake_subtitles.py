"""Stands in for python/subtitles/worker.py: same JSON Lines, no microphone or model."""
import json
import sys


def emit(**value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


emit(type="devices", devices=[{"id": 1, "name": "Mic", "host": "WASAPI"}], gpus=[{"id": 0, "name": "GPU"}])
for line in sys.stdin:
    command = json.loads(line)
    if command.get("action") == "start":
        emit(type="running", value=True)
        emit(type="status", text="字幕を生成中")
        emit(type="level", value=0.5)
        emit(type="subtitle", ja="ファックって言った " + command["model"], en="I said fuck", latency=0.1)
    elif command.get("action") == "stop":
        emit(type="running", value=False)
        emit(type="status", text="停止しました")
