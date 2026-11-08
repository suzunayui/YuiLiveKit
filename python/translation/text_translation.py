"""Language hints and TranslateGemma JSON worker."""
import json
import os
import re
import sys
import unicodedata
from pathlib import Path

# Exact greetings provide source-language hints for very short comments.
# TranslateGemma generates the actual translation, including for these phrases.
# Match complete phrases only so expressions such as "good morning workout"
# still go through the translation model.
ENGLISH_PHRASES = {
    'good morning':'おはようございます', 'good afternoon':'こんにちは',
    'good evening':'こんばんは', 'good night':'おやすみなさい',
    'hello':'こんにちは', 'hi':'こんにちは', 'hey':'こんにちは',
    'thank you':'ありがとうございます', 'thanks':'ありがとう',
}
PHRASE_PATTERN = re.compile(
    r'(?P<prefix>[^\w]*)'
    r'(?P<phrase>good\s+(?:morning|afternoon|evening|night)|thank\s+you|thanks|hello|hi|hey)'
    r'(?:[\s,]+(?P<audience>everyone|everybody|all|chat))?'
    r'(?P<suffix>[^\w]*)', re.IGNORECASE)

MULTILINGUAL_PHRASES = {
    'guten morgen':('de', 'おはようございます'),
    'guten morgen zusammen':('de', '皆さん、おはようございます'),
    'guten tag':('de', 'こんにちは'), 'guten abend':('de', 'こんばんは'),
    'gute nacht':('de', 'おやすみなさい'), 'hallo':('de', 'こんにちは'),
    'danke':('de', 'ありがとう'), 'vielen dank':('de', 'ありがとうございます'),
    'dankeschön':('de', 'ありがとうございます'),
    'bonjour':('fr', 'こんにちは'), 'bonsoir':('fr', 'こんばんは'),
    'bonne nuit':('fr', 'おやすみなさい'), 'merci':('fr', 'ありがとう'),
    'buenos días':('es', 'おはようございます'), 'buenos dias':('es', 'おはようございます'),
    'buenas tardes':('es', 'こんにちは'), 'buenas noches':('es', 'こんばんは'),
    'hola':('es', 'こんにちは'), 'gracias':('es', 'ありがとう'),
    'bom dia':('pt', 'おはようございます'), 'boa tarde':('pt', 'こんにちは'),
    'boa noite':('pt', 'こんばんは'), 'olá':('pt', 'こんにちは'),
    'obrigado':('pt', 'ありがとう'), 'obrigada':('pt', 'ありがとう'),
    'buongiorno':('it', 'おはようございます'), 'buonasera':('it', 'こんばんは'),
    'buonanotte':('it', 'おやすみなさい'), 'grazie':('it', 'ありがとう'),
    'доброе утро':('ru', 'おはようございます'), 'добрый день':('ru', 'こんにちは'),
    'добрый вечер':('ru', 'こんばんは'), 'спокойной ночи':('ru', 'おやすみなさい'),
    'привет':('ru', 'こんにちは'), 'спасибо':('ru', 'ありがとう'),
    '早上好':('zh', 'おはようございます'), '早安':('zh', 'おはようございます'),
    '你好':('zh', 'こんにちは'), '晚上好':('zh', 'こんばんは'),
    '晚安':('zh', 'おやすみなさい'), '谢谢':('zh', 'ありがとう'), '謝謝':('zh', 'ありがとう'),
    '안녕하세요':('ko', 'こんにちは'), '좋은 아침':('ko', 'おはようございます'),
    '감사합니다':('ko', 'ありがとうございます'),
}
MULTILINGUAL_PATTERN = re.compile(
    r'(?P<prefix>[^\w]*)(?P<phrase>' + '|'.join(
        re.escape(phrase).replace(r'\ ', r'\s+')
        for phrase in sorted(MULTILINGUAL_PHRASES, key=len, reverse=True)
    ) + r')(?P<suffix>[^\w]*)', re.IGNORECASE)

def fixed_translation(text, language=None):
    normalized = unicodedata.normalize('NFKC', text)
    match = MULTILINGUAL_PATTERN.fullmatch(normalized)
    if match:
        key = ' '.join(match['phrase'].casefold().split())
        source, ja = MULTILINGUAL_PHRASES[key]
        if language is None or language == source:
            return {'ja':match['prefix'] + ja + match['suffix'], 'language':source}
    if language is None or language == 'en':
        match = PHRASE_PATTERN.fullmatch(normalized)
        if match:
            key = ' '.join(match['phrase'].casefold().split())
            ja = ENGLISH_PHRASES[key]
            if match['audience']:
                ja = '皆さん、' + ja
            return {'ja':match['prefix'] + ja + match['suffix'], 'language':'en'}
    return None

def fixed_phrase(text, language=None):
    result = fixed_translation(text, language)
    return result['ja'] if result is not None else None

def emit(kind, **value):
    print(json.dumps({'type':kind, **value}, ensure_ascii=False), flush=True)

from gemma_translation import TextTranslator

def main():
    translator = None
    try:
        config = json.loads(sys.stdin.readline())
        cache = Path(os.environ.get('SUBTITLE_MODEL_DIR', str(Path(__file__).resolve().parent.parent / 'models')))
        translator = TextTranslator(cache, config.get('device', 'cpu'), lambda text: emit('status', text=text))
        emit('ready')
        for line in sys.stdin:
            command = json.loads(line)
            try:
                emit('result', id=command['id'], **translator.translate(command.get('text', '')))
            except Exception as error:
                emit('translation-error', id=command.get('id'), text=str(error))
    except Exception as error:
        import traceback
        traceback.print_exc(file=sys.stderr)
        emit('error', text=f'ローカル翻訳エラー: {error}')
    finally:
        if translator:
            translator.close()

if __name__ == '__main__':
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        if stream is not None and hasattr(stream, 'reconfigure'):
            stream.reconfigure(encoding='utf-8')
    main()
