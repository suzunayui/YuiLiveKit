"""Stands in for python/translation/worker.py: same JSON Lines, no model."""
import json
import sys


def emit(**value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


config = json.loads(sys.stdin.readline())
emit(type="status", text="device " + config["device"])
emit(type="ready")
for line in sys.stdin:
    command = json.loads(line)
    text = command["text"]
    if text == "fail":
        emit(type="translation-error", id=command["id"], text="failed")
    else:
        emit(type="result", id=command["id"], ja="訳:" + text, language="en")
