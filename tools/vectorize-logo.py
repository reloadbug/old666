"""老六清净 · 商标矢量化管线

从「黑红logo.svg」（内嵌两层 PNG 的黑红扁平标）产出干净矢量：
  1. 抽出内嵌图层 PNG
  2. 按 SVG 内的 <use x y> 合成到完整画布
  3. vtracer 矢量化
  4. 清理：丢弃空路径与碎屑、烘焙 translate、按内容裁剪 viewBox、色值归一

依赖: sharp(node) 用于合成，vtracer(python) 用于矢量化
用法: python tools/vectorize-logo.py
"""

import re
import os
import subprocess
import sys

ROOT = r'D:\AI game\old666'
TMP = os.path.join(ROOT, '.tmp-logo')
SRC = r'D:\images\黑红logo.svg'
NODE = r'C:\Users\Administrator\.workbuddy-ai\binaries\node\versions\22.22.2-6\node.exe'
NODE_MODULES = r'C:\Users\Administrator\.workbuddy-ai\binaries\node\workspace\node_modules'

INK = '#0E1116'
RED = '#F5340F'


def step(msg):
    print('  ' + msg)


def extract_layers():
    """抽出内嵌 PNG，并读取 <use> 的位置"""
    s = open(SRC, encoding='utf-8').read()
    import base64
    layers = []
    for m in re.finditer(r'<image\s+width="(\d+)"\s+height="(\d+)"\s+id="(img\d+)"\s+href="data:image/(\w+);base64,([^"]+)"', s):
        w, h, id_, fmt, b64 = int(m.group(1)), int(m.group(2)), m.group(3), m.group(4), m.group(5)
        p = os.path.join(TMP, '%s.%s' % (id_, fmt))
        open(p, 'wb').write(base64.b64decode(b64))
        layers.append({'id': id_, 'w': w, 'h': h, 'path': p, 'x': 0, 'y': 0})
    for m in re.finditer(r'<use[^>]*href="#(img\d+)"\s+x="(-?\d+)"\s+y="(-?\d+)"', s):
        for L in layers:
            if L['id'] == m.group(1):
                L['x'], L['y'] = int(m.group(2)), int(m.group(3))
    step('抽出 %d 个图层' % len(layers))
    return layers


def composite(layers, vb):
    js = """
const sharp=require('sharp');
(async()=>{
  const L=%s;
  const comp=L.map(l=>({input:l.path,left:l.x,top:l.y}));
  await sharp({create:{width:%d,height:%d,channels:4,background:{r:0,g:0,b:0,alpha:0}}})
    .composite(comp).png().toFile(%s);
  console.log('ok');
})();
""" % (str([{'path': L['path'], 'x': L['x'], 'y': L['y']} for L in layers]).replace("'", '"'),
       vb, vb, '"%s"' % os.path.join(TMP, 'composite.png').replace('\\', '/'))
    env = dict(os.environ, NODE_PATH=NODE_MODULES)
    r = subprocess.run([NODE, '-e', js], env=env, capture_output=True, text=True)
    if r.returncode != 0:
        print(r.stderr)
        sys.exit(1)
    step('合成画布 %dx%d' % (vb, vb))


def vectorize():
    import vtracer
    vtracer.convert_image_to_svg_py(
        os.path.join(TMP, 'composite.png'), os.path.join(TMP, 'traced.svg'),
        colormode='color', hierarchical='stacked', mode='spline',
        filter_speckle=8, color_precision=8, layer_difference=8,
        corner_threshold=60, length_threshold=3.5, max_iterations=10,
        splice_threshold=45, path_precision=2)
    step('vtracer 完成')


PATH_RE = re.compile(r'<path\b([^>]*?)/>')
ATTR_RE = re.compile(r'([\w-]+)="([^"]*)"')
NUM_RE = re.compile(r'-?\d+\.?\d*')


def clean(out_path, ink=INK, red=RED, min_span=3.0):
    s = open(os.path.join(TMP, 'traced.svg'), encoding='utf-8').read()
    kept = []
    dropped_empty = dropped_speck = 0
    for m in PATH_RE.finditer(s):
        at = dict(ATTR_RE.findall(m.group(1)))
        d = at.get('d', '').strip()
        if not d:
            dropped_empty += 1
            continue
        tr = re.search(r'translate\(([-\d.]+)[ ,]+([-\d.]+)\)', at.get('transform', ''))
        tx, ty = (float(tr.group(1)), float(tr.group(2))) if tr else (0.0, 0.0)
        v = [float(x) for x in NUM_RE.findall(d)]
        xs = [v[i] + tx for i in range(0, len(v), 2)]
        ys = [v[i] + ty for i in range(1, len(v), 2)]
        if max(xs) - min(xs) < min_span and max(ys) - min(ys) < min_span:
            dropped_speck += 1
            continue
        # 烘焙 translate：偶数位加 tx，奇数位加 ty
        i = [0]

        def sub(mm):
            val = float(mm.group())
            out = val + (tx if i[0] % 2 == 0 else ty)
            i[0] += 1
            return ('%.1f' % out).rstrip('0').rstrip('.')

        kept.append((NUM_RE.sub(sub, d), at.get('fill', ink).upper()))
    step('丢弃空路径 %d、碎屑 %d，保留 %d' % (dropped_empty, dropped_speck, len(kept)))

    # 色值归一
    def remap(c):
        r = int(c[1:3], 16); g = int(c[3:5], 16); b = int(c[5:7], 16)
        return red if (r > g + 25 and r > b + 25) else ink

    groups = {}
    for d, c in kept:
        groups.setdefault(remap(c), []).append(d)

    # 计算整体 bbox
    allv = []
    for d, _ in kept:
        allv += [float(x) for x in NUM_RE.findall(d)]
    xs, ys = allv[0::2], allv[1::2]
    x0, y0, x1, y1 = min(xs), min(ys), max(xs), max(ys)
    pad = 8
    vbx, vby = x0 - pad, y0 - pad
    vbw, vbh = (x1 - x0) + pad * 2, (y1 - y0) + pad * 2

    body = ''.join('<path fill="%s" d="%s"/>' % (c, ''.join(ds)) for c, ds in groups.items())
    out = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="%.0f %.0f %.0f %.0f" '
           'width="%.0f" height="%.0f" role="img" aria-label="老六清净 图形标">'
           '<title>老六清净 图形标</title>%s</svg>' % (vbx, vby, vbw, vbh, vbw, vbh, body))
    open(out_path, 'w', encoding='utf-8').write(out)
    step('输出 %s  %.1f KB  %d 色  viewBox=%.0f %.0f %.0f %.0f'
         % (os.path.basename(out_path), os.path.getsize(out_path) / 1024, len(groups), vbx, vby, vbw, vbh))
    return vbx, vby, vbw, vbh


if __name__ == '__main__':
    os.makedirs(TMP, exist_ok=True)
    print('矢量化管线:')
    layers = extract_layers()
    composite(layers, 1500)
    vectorize()
    clean(os.path.join(ROOT, 'assets', 'logo', 'mark.svg'))
    print('完成')
