"""Keep quiet consonants and pre-roll; close phrases only after sustained silence."""
from collections import deque
import numpy as np
from faster_whisper.vad import get_vad_model


class LocalVoiceDetector:
    def __init__(self):
        self.model = get_vad_model()
        self.history = deque(maxlen=60)
        self.frame_count = 0
        self.probability = 0.0

    def push(self, frame):
        self.history.append(frame)
        self.frame_count += 1
        if self.frame_count % 4 == 0:
            recent = np.concatenate(self.history)
            recent = np.pad(recent, (0, (-len(recent)) % 512))
            probabilities = self.model(recent)
            self.probability = float(np.max(probabilities[-4:]))
        return self.probability


class SpeechSegmenter:
    def __init__(self, threshold=0.5, frame_seconds=0.03):
        self.threshold = threshold
        self.pre_roll = deque(maxlen=round(0.6 / frame_seconds))
        self.max_frames = round(12 / frame_seconds)
        self.silence_frames = round(0.9 / frame_seconds)
        self.min_voiced_frames = round(0.24 / frame_seconds)
        self.frames = []
        self.quiet = self.voiced = 0

    def push(self, frame, probability):
        is_voice = probability >= (self.threshold - 0.15 if self.frames else self.threshold)
        if not self.frames:
            self.pre_roll.append(frame)
            if not is_voice:
                return None
            self.frames = list(self.pre_roll)
            self.pre_roll.clear()
        else:
            self.frames.append(frame)
        self.quiet = 0 if is_voice else self.quiet + 1
        self.voiced += int(is_voice)
        if self.quiet < self.silence_frames and len(self.frames) < self.max_frames:
            return None
        audio = np.concatenate(self.frames) if self.voiced >= self.min_voiced_frames else None
        self.frames = []
        self.quiet = self.voiced = 0
        return audio
