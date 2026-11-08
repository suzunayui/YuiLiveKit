"""Standalone text translation worker; no microphone or Whisper dependencies."""
import sys
from text_translation import main
for stream in (sys.stdin, sys.stdout, sys.stderr):
    if stream is not None and hasattr(stream, 'reconfigure'):
        stream.reconfigure(encoding='utf-8')
if __name__ == '__main__':
    main()
