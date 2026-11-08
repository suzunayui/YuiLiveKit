"""User-specified readings for names that should not be freely translated."""
import re
import unicodedata

NAME_PATTERN = re.compile(r'(?<![\w])suzunayui(?![\w])', re.IGNORECASE)
NAME_ONLY = re.compile(r'(?P<prefix>[^\w]*)suzunayui(?P<suffix>[^\w]*)', re.IGNORECASE)


def name_translation(text):
    match = NAME_ONLY.fullmatch(unicodedata.normalize('NFKC', text))
    if match:
        return {'ja': match['prefix'] + 'すずなゆい' + match['suffix'], 'language': 'en'}
    return None


def name_glossary(text):
    if NAME_PATTERN.search(unicodedata.normalize('NFKC', text)):
        return 'The proper name "suzunayui" must be rendered as "すずなゆい" in Japanese.\n'
    return ''
