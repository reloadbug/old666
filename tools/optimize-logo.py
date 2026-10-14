"""老六清净 · 商标 SVG 优化管线

把自动描摹产生的超大 SVG（1319 路径 / 925KB）加工成产品可用的图标资源。

用法:
    python tools/optimize-logo.py analyze     # 只分析，不改文件
    python tools/optimize-logo.py build       # 产出优化后的资源
"""

import re
import sys
import os
from collections import defaultdict, Counter

SRC = r'D:\images\reloadbug.svg'
OUT_DIR = r'D:\AI game\old666\assets\logo'

PATH_RE = re.compile(r'<path\s+fill="(#[0-9A-Fa-f]{6})"[^>]*?\sd="(.*?)"\s*></path>', re.S)
NUM_RE = re.compile(r'-?\d+\.?\d*')


def fmt(v):
    if abs(v - round(v)) < 1e-9:
        return str(int(round(v)))
    return ('%.2f' % v).rstrip('0').rstrip('.')


def clean_d(d):
    return NUM_RE.sub(lambda m: fmt(float(m.group())), d)


def nums(d):
    return [float(x) for x in NUM_RE.findall(d)]


def bbox(d):
    v = nums(d)
    xs, ys = v[0::2], v[1::2]
    return min(xs), min(ys), max(xs), max(ys)


def is_warm(hexc):
    r = int(hexc[1:3], 16); g = int(hexc[3:5], 16); b = int(hexc[5:7], 16)
    return r > g + 25 and r > b + 25


def lum(hexc):
    r = int(hexc[1:3], 16); g = int(hexc[3:5], 16); b = int(hexc[5:7], 16)
    return 0.299 * r + 0.587 * g + 0.114 * b


def load():
    svg = open(SRC, encoding='utf-8').read()
    paths = [(m.group(1).upper(), m.group(2)) for m in PATH_RE.finditer(svg)]
    return paths


def analyze():
    paths = load()
    print('路径总数        : %d' % len(paths))
    print('唯一填充色      : %d' % len(set(c for c, _ in paths)))
    print('六位小数坐标数  : %d' % len(NUM_RE.findall(''.join(d for _, d in paths))))

    warm = [(c, d) for c, d in paths if is_warm(c)]
    gray = [(c, d) for c, d in paths if not is_warm(c)]
    print('暖色（眼睛）路径: %d' % len(warm))
    print('灰阶路径        : %d' % len(gray))

    # 白色路径：是否触碰画布边缘
    whites = [(c, d) for c, d in gray if c == '#FFFFFF']
    edge, inner = 0, 0
    for c, d in whites:
        x0, y0, x1, y1 = bbox(d)
        if x0 < 3 or y0 < 3 or x1 > 997 or y1 > 997:
            edge += 1
        else:
            inner += 1
    print('白色路径        : %d（贴边=背景 %d，内部= %d）' % (len(whites), edge, inner))

    # 灰阶亮度直方图（每 16 一档）
    hist = Counter()
    for c, _ in gray:
        if c == '#FFFFFF':
            continue
        hist[int(lum(c)) // 16 * 16] += 1
    print('\n灰阶亮度分布（非白）:')
    for k in sorted(hist):
        print('  %3d-%3d : %s %d' % (k, k + 15, '#' * min(60, hist[k] // 2), hist[k]))


def build():
    paths = load()
    os.makedirs(OUT_DIR, exist_ok=True)

    kept, dropped = [], 0
    for c, d in paths:
        if c == '#FFFFFF':
            x0, y0, x1, y1 = bbox(d)
            if x0 < 3 or y0 < 3 or x1 > 997 or y1 > 997:
                dropped += 1
                continue
        kept.append((c, d))
    print('去除背景白路径: %d，保留: %d' % (dropped, len(kept)))

    # ---- A. 高保真版：保留全部色彩，按颜色合并，降低精度 ----
    groups = defaultdict(list)
    for c, d in kept:
        groups[c].append(clean_d(d))
    parts = []
    for c, ds in groups.items():
        parts.append('<path fill="%s" d="%s"/>' % (c, ''.join(ds)))
    hd = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 1000" '
          'width="1000" height="1000" role="img" aria-label="老六清净 图形标">'
          '<title>老六清净 图形标</title>' + ''.join(parts) + '</svg>')
    p_hd = os.path.join(OUT_DIR, 'mark.svg')
    open(p_hd, 'w', encoding='utf-8').write(hd)
    print('  mark.svg        (高保真) %6.1f KB  %d 色' % (os.path.getsize(p_hd) / 1024, len(groups)))

    # ---- B. 扁平版：墨黑 + 赤红，透明底，用于小尺寸与深色背景 ----
    INK, RED = '#0E1116', '#F5340F'
    TH = 205  # 亮度阈值：低于此值视为墨黑
    ink_d, red_d = [], []
    for c, d in kept:
        if is_warm(c):
            red_d.append(clean_d(d))
        elif c != '#FFFFFF' and lum(c) < TH:
            ink_d.append(clean_d(d))
    flat = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 1000" '
            'width="1000" height="1000" role="img" aria-label="老六清净 图形标（扁平版）">'
            '<title>老六清净 图形标（扁平版）</title>'
            '<path fill="%s" d="%s"/><path fill="%s" d="%s"/></svg>'
            % (INK, ''.join(ink_d), RED, ''.join(red_d)))
    p_flat = os.path.join(OUT_DIR, 'mark-flat.svg')
    open(p_flat, 'w', encoding='utf-8').write(flat)
    print('  mark-flat.svg   (扁平)   %6.1f KB  %d 色' % (os.path.getsize(p_flat) / 1024, 2))

    # ---- C. 单色版：全墨黑，用于印章 / 水印 ----
    mono = flat.replace(RED, INK).replace('（扁平版）', '（单色）')
    p_mono = os.path.join(OUT_DIR, 'mark-mono-dark.svg')
    open(p_mono, 'w', encoding='utf-8').write(mono)
    print('  mark-mono-dark.svg       %6.1f KB' % (os.path.getsize(p_mono) / 1024))

    # ---- D. 纯白单色版：深色背景用 ----
    white = mono.replace(INK, '#FFFFFF').replace('（单色）', '（单色·纯白）')
    p_w = os.path.join(OUT_DIR, 'mark-mono-light.svg')
    open(p_w, 'w', encoding='utf-8').write(white)
    print('  mark-mono-light.svg      %6.1f KB' % (os.path.getsize(p_w) / 1024))

    print('\n原始文件: %.1f KB' % (os.path.getsize(SRC) / 1024))


if __name__ == '__main__':
    cmd = sys.argv[1] if len(sys.argv) > 1 else 'analyze'
    if cmd == 'analyze':
        analyze()
    elif cmd == 'build':
        build()
    else:
        print(__doc__)
