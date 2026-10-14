"""老六清净 · 品牌资源组装

用优化后的图形标组装组合标与应用图标（自动按内容包围盒定位缩放）。

用法: python tools/build-brand-assets.py
"""

import re
import os

LOGO = r'D:\AI game\old666\assets\logo'
NUM = re.compile(r'-?\d+\.?\d*')
D_ATTR = re.compile(r'\sd="([^"]*)"')


def inner_of(fn):
    s = open(os.path.join(LOGO, fn), encoding='utf-8').read()
    return re.search(r'<svg[^>]*>(.*)</svg>', s, re.S).group(1)


def bbox(svg_frag):
    """只从 d 属性取坐标，避免 fill 色值被误当数字"""
    v = []
    for m in D_ATTR.finditer(svg_frag):
        v += [float(x) for x in NUM.findall(m.group(1))]
    xs, ys = v[0::2], v[1::2]
    return min(xs), min(ys), max(xs), max(ys)


def fit(fn, bx, by, bw, bh):
    """把图形标按内容包围盒等比缩放并居中到目标矩形"""
    inner = inner_of(fn)
    x0, y0, x1, y1 = bbox(inner)
    s = min(bw / (x1 - x0), bh / (y1 - y0))
    tx = bx + (bw - (x1 - x0) * s) / 2 - x0 * s
    ty = by + (bh - (y1 - y0) * s) / 2 - y0 * s
    return '<g transform="translate(%.2f,%.2f) scale(%.5f)">%s</g>' % (tx, ty, s, inner)


FONT_CN = "'Microsoft YaHei','PingFang SC','Hiragino Sans GB','Noto Sans SC',sans-serif"
FONT_EN = "'Segoe UI','Helvetica Neue',Arial,sans-serif"
WORD_GRAD = ('<linearGradient id="wordGrad" x1="0" y1="0" x2="1" y2="0">'
             '<stop offset="0" stop-color="#0EA5A4"/><stop offset="1" stop-color="#22C55E"/>'
             '</linearGradient>')


def write(fn, body):
    open(os.path.join(LOGO, fn), 'w', encoding='utf-8').write(body)
    print('  %-24s %6.1f KB' % (fn, os.path.getsize(os.path.join(LOGO, fn)) / 1024))


# ---------- 横版组合标 ----------
g = fit('mark-flat.svg', 50, 60, 470, 480)
write('lockup-horizontal.svg',
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1180 600" width="1180" height="600" '
      'role="img" aria-label="老六清净 old666 横版组合标"><title>老六清净 old666 横版组合标</title>'
      '<desc>图形标在左，中文名与英文标识在右。</desc><defs>%s</defs>%s'
      '<text x="590" y="320" font-family="%s" font-size="120" font-weight="800" fill="#0E1116" '
      'letter-spacing="6">老六清净</text>'
      '<text x="594" y="414" font-family="%s" font-size="58" font-weight="600" fill="url(#wordGrad)" '
      'letter-spacing="3.5">old666</text></svg>'
      % (WORD_GRAD, g, FONT_CN, FONT_EN))

# ---------- 竖版组合标 ----------
g = fit('mark-flat.svg', 150, 20, 500, 520)
write('lockup-vertical.svg',
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 920" width="800" height="920" '
      'role="img" aria-label="老六清净 old666 竖版组合标"><title>老六清净 old666 竖版组合标</title>'
      '<desc>图形标在上，中文名与英文标识居中在下。</desc><defs>%s</defs>%s'
      '<text x="400" y="786" text-anchor="middle" font-family="%s" font-size="112" font-weight="800" '
      'fill="#0E1116" letter-spacing="8">老六清净</text>'
      '<text x="400" y="868" text-anchor="middle" font-family="%s" font-size="52" font-weight="600" '
      'fill="url(#wordGrad)" letter-spacing="4">old666</text></svg>'
      % (WORD_GRAD, g, FONT_CN, FONT_EN))

# ---------- 应用图标 · 深色 ----------
g = fit('mark-dark.svg', 0, 0, 1024, 1024)
g = '<g transform="translate(512,512) scale(0.78) translate(-512,-512)">%s</g>' % g
write('app-icon.svg',
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024" '
      'role="img" aria-label="老六清净 应用图标（深色）"><title>老六清净 应用图标（深色）</title>'
      '<defs><linearGradient id="bgDark" x1="0" y1="0" x2="0" y2="1">'
      '<stop offset="0" stop-color="#1E2836"/><stop offset="1" stop-color="#070A0F"/></linearGradient></defs>'
      '<rect width="1024" height="1024" rx="228" fill="url(#bgDark)"/>%s</svg>' % g)

# ---------- 应用图标 · 浅色 ----------
g = fit('mark.svg', 0, 0, 1024, 1024)
g = '<g transform="translate(512,512) scale(0.78) translate(-512,-512)">%s</g>' % g
write('app-icon-light.svg',
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024" '
      'role="img" aria-label="老六清净 应用图标（浅色）"><title>老六清净 应用图标（浅色）</title>'
      '<defs><linearGradient id="bgLight" x1="0" y1="0" x2="0" y2="1">'
      '<stop offset="0" stop-color="#FFFFFF"/><stop offset="1" stop-color="#EAF3F2"/></linearGradient></defs>'
      '<rect width="1024" height="1024" rx="228" fill="url(#bgLight)"/>'
      '<rect x="1" y="1" width="1022" height="1022" rx="227" fill="none" stroke="#DCE6E6" stroke-width="2"/>'
      '%s</svg>' % g)

print('完成')
