"""老六清净 · 商标 SVG 路径简化器

自动描摹的 SVG 有约 37,000 个点，远超图标所需。本脚本：
  1. 解析全部路径指令（M/L/C/A/z，含相对与绝对）
  2. 圆弧转三次贝塞尔，贝塞尔自适应展平为折线
  3. Douglas-Peucker 抽稀
  4. 按填充色合并子路径

用法:
    python tools/simplify-logo.py <tolerance> <输出文件>
"""

import re
import sys
import os
import math
from collections import defaultdict

TOKEN_RE = re.compile(r'([MmLlHhVvCcSsQqTtAaZz])|(-?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?)')
ARGC = {'M': 2, 'L': 2, 'H': 1, 'V': 1, 'C': 6, 'S': 4, 'Q': 4, 'T': 2, 'A': 7, 'Z': 0}


def tokenize(d):
    out = []
    for m in TOKEN_RE.finditer(d):
        if m.group(1):
            out.append(m.group(1))
        else:
            out.append(float(m.group(2)))
    return out


def pt_seg_dist(p, a, b):
    px, py = p
    ax, ay = a
    bx, by = b
    dx, dy = bx - ax, by - ay
    if dx == 0 and dy == 0:
        return ((px - ax) ** 2 + (py - ay) ** 2) ** 0.5
    t = ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)
    t = max(0.0, min(1.0, t))
    cx, cy = ax + t * dx, ay + t * dy
    return ((px - cx) ** 2 + (py - cy) ** 2) ** 0.5


def mid(a, b):
    return ((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0)


def flatten_cubic(p0, p1, p2, p3, tol, out, depth=0):
    if depth >= 14:
        out.append(p3)
        return
    if max(pt_seg_dist(p1, p0, p3), pt_seg_dist(p2, p0, p3)) <= tol:
        out.append(p3)
        return
    p01, p12, p23 = mid(p0, p1), mid(p1, p2), mid(p2, p3)
    p012, p123 = mid(p01, p12), mid(p12, p23)
    p0123 = mid(p012, p123)
    flatten_cubic(p0, p01, p012, p0123, tol, out, depth + 1)
    flatten_cubic(p0123, p123, p23, p3, tol, out, depth + 1)


def arc_to_cubics(x0, y0, rx, ry, phi, laf, sf, x1, y1, out, tol):
    """端点式圆弧 -> 一组三次贝塞尔（标准实现）"""
    if rx == 0 or ry == 0:
        out.append((x1, y1))
        return
    rx, ry = abs(rx), abs(ry)
    phi = math.radians(phi)
    cosp, sinp = math.cos(phi), math.sin(phi)
    dx2, dy2 = (x0 - x1) / 2.0, (y0 - y1) / 2.0
    x1p = cosp * dx2 + sinp * dy2
    y1p = -sinp * dx2 + cosp * dy2
    lam = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry)
    if lam > 1:
        s = lam ** 0.5
        rx *= s
        ry *= s
    num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p
    den = rx * rx * y1p * y1p + ry * ry * x1p * x1p
    co = (max(0.0, num / den)) ** 0.5
    if laf == sf:
        co = -co
    cxp = co * rx * y1p / ry
    cyp = -co * ry * x1p / rx
    cx = cosp * cxp - sinp * cyp + (x0 + x1) / 2.0
    cy = sinp * cxp + cosp * cyp + (y0 + y1) / 2.0

    def ang(ux, uy, vx, vy):
        d = (ux * vx + uy * vy) / (((ux * ux + uy * uy) ** 0.5) * ((vx * vx + vy * vy) ** 0.5) + 1e-12)
        d = max(-1.0, min(1.0, d))
        a = math.acos(d)
        return -a if (ux * vy - uy * vx) < 0 else a

    th1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dth = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if sf == 0 and dth > 0:
        dth -= 2 * math.pi
    elif sf == 1 and dth < 0:
        dth += 2 * math.pi

    n = max(1, int(math.ceil(abs(dth) / (math.pi / 2))))
    step = dth / n
    k = 4.0 / 3.0 * math.tan(step / 4.0)
    px, py = x0, y0
    for i in range(n):
        t1 = th1 + i * step
        t2 = t1 + step
        c1 = math.cos(t1); s1 = math.sin(t1)
        c2 = math.cos(t2); s2 = math.sin(t2)
        ex = cosp * rx * c2 - sinp * ry * s2 + cx
        ey = sinp * rx * c2 + cosp * ry * s2 + cy
        d1x = -cosp * rx * s1 - sinp * ry * c1
        d1y = -sinp * rx * s1 + cosp * ry * c1
        d2x = -cosp * rx * s2 - sinp * ry * c2
        d2y = -sinp * rx * s2 + cosp * ry * c2
        flatten_cubic((px, py), (px + k * d1x, py + k * d1y),
                      (ex - k * d2x, ey - k * d2y), (ex, ey), tol, out)
        px, py = ex, ey


def parse_subpaths(d, tol):
    """返回 [[(x,y), ...], ...]"""
    tk = tokenize(d)
    i = 0
    subpaths = []
    cur = []
    cx = cy = 0.0
    start = (0.0, 0.0)
    prev_ctrl = None
    prev_cmd = ''
    cmd = None
    while i < len(tk):
        if isinstance(tk[i], str):
            cmd = tk[i]
            i += 1
        elif cmd is None:
            break
        c = cmd
        up = c.upper()
        rel = c.islower()
        if up == 'Z':
            if cur:
                cur.append(start)
                subpaths.append(cur)
                cur = []
            cx, cy = start
            cmd = None
            prev_ctrl = None
            continue
        n = ARGC[up]
        if i + n > len(tk):
            break
        a = tk[i:i + n]
        i += n

        if up == 'M':
            if cur:
                subpaths.append(cur)
            x, y = (cx + a[0], cy + a[1]) if rel else (a[0], a[1])
            cx, cy = x, y
            start = (x, y)
            cur = [(x, y)]
            cmd = 'l' if rel else 'L'
            prev_ctrl = None
        elif up == 'L':
            x, y = (cx + a[0], cy + a[1]) if rel else (a[0], a[1])
            cur.append((x, y))
            cx, cy = x, y
            prev_ctrl = None
        elif up == 'H':
            x = cx + a[0] if rel else a[0]
            cur.append((x, cy))
            cx = x
            prev_ctrl = None
        elif up == 'V':
            y = cy + a[0] if rel else a[0]
            cur.append((cx, y))
            cy = y
            prev_ctrl = None
        elif up == 'C':
            if rel:
                p1 = (cx + a[0], cy + a[1]); p2 = (cx + a[2], cy + a[3]); p3 = (cx + a[4], cy + a[5])
            else:
                p1 = (a[0], a[1]); p2 = (a[2], a[3]); p3 = (a[4], a[5])
            flatten_cubic((cx, cy), p1, p2, p3, tol, cur)
            prev_ctrl = p2
            cx, cy = p3
        elif up == 'S':
            if prev_ctrl and prev_cmd.upper() in ('C', 'S'):
                p1 = (2 * cx - prev_ctrl[0], 2 * cy - prev_ctrl[1])
            else:
                p1 = (cx, cy)
            if rel:
                p2 = (cx + a[0], cy + a[1]); p3 = (cx + a[2], cy + a[3])
            else:
                p2 = (a[0], a[1]); p3 = (a[2], a[3])
            flatten_cubic((cx, cy), p1, p2, p3, tol, cur)
            prev_ctrl = p2
            cx, cy = p3
        elif up == 'Q':
            if rel:
                q1 = (cx + a[0], cy + a[1]); p3 = (cx + a[2], cy + a[3])
            else:
                q1 = (a[0], a[1]); p3 = (a[2], a[3])
            p1 = (cx + 2.0 / 3 * (q1[0] - cx), cy + 2.0 / 3 * (q1[1] - cy))
            p2 = (p3[0] + 2.0 / 3 * (q1[0] - p3[0]), p3[1] + 2.0 / 3 * (q1[1] - p3[1]))
            flatten_cubic((cx, cy), p1, p2, p3, tol, cur)
            prev_ctrl = q1
            cx, cy = p3
        elif up == 'T':
            if prev_ctrl and prev_cmd.upper() in ('Q', 'T'):
                q1 = (2 * cx - prev_ctrl[0], 2 * cy - prev_ctrl[1])
            else:
                q1 = (cx, cy)
            x, y = (cx + a[0], cy + a[1]) if rel else (a[0], a[1])
            p1 = (cx + 2.0 / 3 * (q1[0] - cx), cy + 2.0 / 3 * (q1[1] - cy))
            p2 = (x + 2.0 / 3 * (q1[0] - x), y + 2.0 / 3 * (q1[1] - y))
            flatten_cubic((cx, cy), p1, p2, (x, y), tol, cur)
            prev_ctrl = q1
            cx, cy = x, y
        elif up == 'A':
            x, y = (cx + a[5], cy + a[6]) if rel else (a[5], a[6])
            arc_to_cubics(cx, cy, a[0], a[1], a[2], int(a[3]), int(a[4]), x, y, cur, tol)
            cx, cy = x, y
            prev_ctrl = None
        prev_cmd = c
    if cur:
        subpaths.append(cur)
    return subpaths


def dp(points, tol):
    n = len(points)
    if n < 3:
        return points
    keep = bytearray(n)
    keep[0] = keep[n - 1] = 1
    stack = [(0, n - 1)]
    while stack:
        i, j = stack.pop()
        if j <= i + 1:
            continue
        ax, ay = points[i]
        bx, by = points[j]
        maxd, idx = -1.0, -1
        for k in range(i + 1, j):
            d = pt_seg_dist(points[k], points[i], points[j])
            if d > maxd:
                maxd, idx = d, k
        if maxd > tol:
            keep[idx] = 1
            stack.append((i, idx))
            stack.append((idx, j))
    return [p for p, k in zip(points, keep) if k]


def fnum(v):
    if abs(v - round(v)) < 1e-9:
        return str(int(round(v)))
    return ('%.1f' % v).rstrip('0').rstrip('.')


def main():
    src = sys.argv[1]
    tol_flat = float(sys.argv[2])
    tol_dp = float(sys.argv[3])
    out_path = sys.argv[4]
    mode = sys.argv[5] if len(sys.argv) > 5 else 'color'

    INK, RED = '#0E1116', '#F5340F'

    def warm(hexc):
        r = int(hexc[1:3], 16); g = int(hexc[3:5], 16); b = int(hexc[5:7], 16)
        return r > g + 25 and r > b + 25

    def invert(hexc):
        r = int(hexc[1:3], 16); g = int(hexc[3:5], 16); b = int(hexc[5:7], 16)
        return '#%02X%02X%02X' % (255 - r, 255 - g, 255 - b)

    def thresh(hexc, t):
        r = int(hexc[1:3], 16); g = int(hexc[3:5], 16); b = int(hexc[5:7], 16)
        l = 0.299 * r + 0.587 * g + 0.114 * b
        return INK if l < t else None

    def touches_edge(d):
        v = [float(x) for x in re.findall(r'-?\d+\.?\d*', d)]
        xs, ys = v[0::2], v[1::2]
        return min(xs) < 3 or min(ys) < 3 or max(xs) > 997 or max(ys) > 997

    svg = open(src, encoding='utf-8').read()
    paths = re.findall(r'<path\s+fill="(#[0-9A-Fa-f]{6})"[^>]*?\sd="(.*?)"\s*></path>', svg, re.S)

    by_color = defaultdict(list)
    n_in = n_out = 0
    for color, d in paths:
        color = color.upper()
        if color == '#FFFFFF' and touches_edge(d):
            continue
        if mode == 'flat':
            if color == '#FFFFFF':
                continue
            color = RED if warm(color) else INK
        elif mode.startswith('thresh'):
            t = float(mode.split(':')[1]) if ':' in mode else 150.0
            if warm(color):
                color = RED
            else:
                color = thresh(color, t)
                if color is None:
                    continue
        elif mode == 'invert':
            color = '#F5340F' if warm(color) else invert(color)
        for sp in parse_subpaths(d, tol_flat):
            n_in += len(sp)
            s = dp(sp, tol_dp)
            n_out += len(s)
            if len(s) >= 3:
                by_color[color].append(s)

    parts = []
    for color, subs in by_color.items():
        seg = []
        for s in subs:
            seg.append('M' + ' '.join('%s %s' % (fnum(x), fnum(y)) for x, y in s) + 'z')
        parts.append('<path fill="%s" d="%s"/>' % (color, ''.join(seg)))

    out = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 1000" width="1000" height="1000" '
           'role="img" aria-label="老六清净 图形标"><title>老六清净 图形标</title>'
           + ''.join(parts) + '</svg>')
    open(out_path, 'w', encoding='utf-8').write(out)
    print('展平后点数 %d -> 抽稀后 %d (%.1f%%)' % (n_in, n_out, 100.0 * n_out / max(1, n_in)))
    print('输出: %s  %.1f KB  %d 色' % (out_path, os.path.getsize(out_path) / 1024, len(by_color)))


if __name__ == '__main__':
    main()
