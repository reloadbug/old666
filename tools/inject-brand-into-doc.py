"""把真实图形标注入 docs/产品设计方案.html（替换早期手绘占位标）

可重复运行：会更新已存在的 symbol 与各处 <use> 引用。
用法: python tools/inject-brand-into-doc.py
"""

import re
import os

DOC = r'D:\AI game\old666\docs\产品设计方案.html'
MARK = r'D:\AI game\old666\assets\logo\mark.svg'

svg = open(MARK, encoding='utf-8').read()
inner = re.search(r'<svg[^>]*>(.*)</svg>', svg, re.S).group(1)
vb = re.search(r'viewBox="([^"]+)"', svg).group(1)
inner = re.sub(r'id="([^"]+)"', r'id="bm_\1"', inner)
inner = re.sub(r'url\(#([^)]+)\)', r'url(#bm_\1)', inner)

doc = open(DOC, encoding='utf-8').read()

SYMBOL = '<symbol id="brandMark" viewBox="%s">%s</symbol>' % (vb, inner)
WRAP = ('<svg width="0" height="0" style="position:absolute" aria-hidden="true">'
        '<defs>%s</defs></svg>' % SYMBOL)

# 1) symbol：已存在则替换，否则注入
if 'id="brandMark"' in doc:
    doc = re.sub(r'<symbol id="brandMark" viewBox="[^"]*">.*?</symbol>', SYMBOL, doc, count=1, flags=re.S)
else:
    doc = doc.replace('<body>', '<body>\n' + WRAP, 1)

# 2) 页眉 logo（统一到当前 viewBox）
doc = re.sub(r'<div class="logo">\s*<svg.*?</svg>\s*</div>',
             '<div class="logo"><svg viewBox="%s" role="img" aria-label="老六清净 图形标">'
             '<use href="#brandMark"/></svg></div>' % vb,
             doc, count=1, flags=re.S)

# 3) 界面原型标题栏小标志
doc = re.sub(r'<svg class="tb-mk"[^>]*>.*?</svg>',
             '<svg class="tb-mk" viewBox="%s" aria-hidden="true"><use href="#brandMark"/></svg>' % vb,
             doc, flags=re.S)

# 4) 4.2 品牌标志区块里的组合标
lockup = ('<svg style="width:380px;height:auto;flex:none" viewBox="0 0 1180 600" role="img" '
          'aria-label="老六清净 old666 横版组合标">'
          '<defs><linearGradient id="docWordGrad" x1="0" y1="0" x2="1" y2="0">'
          '<stop offset="0" stop-color="#0EA5A4"/><stop offset="1" stop-color="#22C55E"/>'
          '</linearGradient></defs>'
          '<use href="#brandMark" x="50" y="60" width="470" height="480"/>'
          '<text x="590" y="320" font-family="\'Microsoft YaHei\',\'PingFang SC\',sans-serif" '
          'font-size="120" font-weight="800" fill="#0E1116" letter-spacing="6">老六清净</text>'
          '<text x="594" y="414" font-family="\'Segoe UI\',Arial,sans-serif" font-size="58" '
          'font-weight="600" fill="url(#docWordGrad)" letter-spacing="3.5">old666</text></svg>')
doc = re.sub(r'<svg style="width:380px;height:auto;flex:none".*?</svg>', lockup, doc, count=1, flags=re.S)

open(DOC, 'w', encoding='utf-8').write(doc)
print('注入完成，文档 %.1f KB，brandMark 引用 %d 处'
      % (os.path.getsize(DOC) / 1024, len(re.findall(r'href="#brandMark"', doc))))
