"""Frozen 2026-09-27 numeric-format supplement; never used as ASR input.

Numeric normalization used only for a separately reported diagnostic score.
This is a formatting supplement, not semantic equivalence.
"""
import json
import re
import sys
from pathlib import Path

import evaluate_personal as scoring
import evaluate

DIGITS = dict(zip('零〇一二三四五六七八九', [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]))
DIGITS['两'] = 2
UNITS = {'十': 10, '百': 100, '千': 1000, '万': 10000, '亿': 100000000}
NUM = re.compile('[零〇一二两三四五六七八九十百千万亿]+(?:点[零〇一二三四五六七八九]+)*')


def integer(text):
    if not any(c in UNITS for c in text):
        # 一两 is colloquial "one or two", not the integer 12.
        if len(text) > 1 and '两' in text:
            return text
        return ''.join(str(DIGITS[c]) for c in text)
    total = section = value = 0
    for c in text:
        if c in DIGITS:
            value = DIGITS[c]
        elif UNITS[c] < 10000:
            section += (value or 1) * UNITS[c]
            value = 0
        else:
            total += (section + value) * UNITS[c]
            section = value = 0
    return str(total + section + value)


def numeric_surface(text):
    text = scoring.surface(text)
    text = re.sub(r'([0-9]+(?:\.[0-9]+)*)\s*%', r'百分之\1', text)
    text = NUM.sub(lambda m: '点'.join(integer(part) for part in m.group().split('点')), text)
    # Preserve decimal/version boundaries rather than dropping their punctuation.
    text = re.sub(r'(?<=[0-9])\.(?=[0-9])', '点', text)
    return text


def supplemental(rows):
    results = []
    for row in rows:
        ref = evaluate.to_chars(evaluate.normalize(numeric_surface(row['reference'])))
        hyp = evaluate.to_chars(evaluate.normalize(numeric_surface(row['text'])))
        errors = evaluate.levenshtein(ref, hyp)[0]
        results.append({'id': row['id'], 'category': row['category'], 'reference_units': len(ref),
                        'errors': errors, 'literal_errors': row['character_errors']})
    units = sum(r['reference_units'] for r in results)
    errors = sum(r['errors'] for r in results)
    return {'numeric_equivalent_cer_percent': 100 * errors / units if units else None,
            'errors': errors, 'reference_units': units, 'cases': results,
            'limits': 'Supplement only: fixed Chinese/Arabic number and decimal/version formatting. '
                      'Not a semantic score; no name/negation/code equivalence. Literal CER remains primary evidence.'}


def self_check():
    for left, right in [('三十二', '32'), ('零點零一', '0.01'), ('五百', '500'),
                        ('一點二點三', '1.2.3'), ('百分之九十七點五', '97.5%'), ('兩個', '2個')]:
        assert numeric_surface(left) == numeric_surface(right), (left, right)
    for left, right in [('十', '一'), ('零點一', '零點零一'), ('不是二十', '是二十'),
                        ('一點二點三', '一百二十三'), ('一兩個', '十二個')]:
        assert numeric_surface(left) != numeric_surface(right), (left, right)


if __name__ == '__main__':
    self_check()
    for filename in sys.argv[1:]:
        path = Path(filename)
        data = json.loads(path.read_text())
        rows = data.get('cases', [])
        if data.get('status') != 'complete' or not rows:
            print(f'{path.name}: incomplete, skipped')
            continue
        summary = supplemental(rows)
        target = path.with_name(path.stem + '-numeric-analysis.json')
        target.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
        print(path.name, json.dumps({k: v for k, v in summary.items() if k not in {'cases', 'limits'}}))
