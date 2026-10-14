"""老六清净 · 品牌规范页生成器

读取 assets/logo 下的真实 SVG 资源，生成自包含的 showcase.html。
用法: python tools/build-brand-pages.py
"""

import re
import os

LOGO = r'D:\AI game\old666\assets\logo'
OUT = os.path.join(LOGO, 'showcase.html')

ASSETS = [
    ('mark', 'mark.svg'),
    ('markDark', 'mark-dark.svg'),
    ('markGlossy', 'mark-glossy.svg'),
    ('monoDark', 'mark-mono-dark.svg'),
    ('monoLight', 'mark-mono-light.svg'),
    ('lkH', 'lockup-horizontal.svg'),
    ('lkV', 'lockup-vertical.svg'),
    ('appDark', 'app-icon.svg'),
    ('appLight', 'app-icon-light.svg'),
]


def read(fn):
    return open(os.path.join(LOGO, fn), encoding='utf-8').read()


def inner(svg):
    return re.search(r'<svg[^>]*>(.*)</svg>', svg, re.S).group(1)


def ns(text, prefix):
    """给内部 id 与引用加前缀，避免多个 symbol 冲突"""
    text = re.sub(r'id="([^"]+)"', lambda m: 'id="%s_%s"' % (prefix, m.group(1)), text)
    text = re.sub(r'url\(#([^)]+)\)', lambda m: 'url(#%s_%s)' % (prefix, m.group(1)), text)
    return text


symbols = []
meta = {}
for key, fn in ASSETS:
    svg = read(fn)
    vb = re.search(r'viewBox="([^"]+)"', svg).group(1)
    symbols.append('<symbol id="%s" viewBox="%s">%s</symbol>' % (key, vb, ns(inner(svg), key)))
    meta[key] = os.path.getsize(os.path.join(LOGO, fn)) / 1024.0

SYMDEF = '<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs>%s</defs></svg>' % ''.join(symbols)

size_rows = '\n'.join(
    '        <tr><td><code>%s</code></td><td>%s</td><td>%.1f KB</td></tr>' % (fn, desc, meta[key])
    for key, fn, desc in [
        ('mark', 'mark.svg', '图形标 · 主用（黑 + 赤红，矢量化自 黑红logo.svg）'),
        ('markDark', 'mark-dark.svg', '图形标 · 深色背景版（白 + 赤红）'),
        ('markGlossy', 'mark-glossy.svg', '图形标 · 光泽版（大尺寸展示可选）'),
        ('monoDark', 'mark-mono-dark.svg', '图形标 · 单色墨黑（印章 / 水印）'),
        ('monoLight', 'mark-mono-light.svg', '图形标 · 单色纯白（深底）'),
        ('lkH', 'lockup-horizontal.svg', '横版组合标'),
        ('lkV', 'lockup-vertical.svg', '竖版组合标'),
        ('appDark', 'app-icon.svg', '应用图标 · 深色'),
        ('appLight', 'app-icon-light.svg', '应用图标 · 浅色'),
    ])

HTML = '''<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>老六清净 old666 · 品牌标志规范</title>
<style>
  :root{
    --bg:#F4F8F8; --surface:#FFFFFF; --surface-2:#F7FBFA;
    --ink:#0E1116; --primary:#0EA5A4; --primary-d:#0F766E; --primary-l:#CCFBF1;
    --green:#22C55E; --red:#F5340F;
    --text:#0F172A; --muted:#64748B; --line:#E2E8F0; --line-2:#EFF3F4;
    --shadow:0 1px 2px rgba(15,23,42,.04), 0 8px 24px rgba(15,23,42,.06);
  }
  *{box-sizing:border-box;margin:0;padding:0}
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI","Microsoft YaHei","PingFang SC",sans-serif;
    background:radial-gradient(1100px 560px at 8% -8%,#D9F5F1 0%,transparent 55%),
      radial-gradient(900px 460px at 100% 0%,#E4EEFF 0%,transparent 50%),var(--bg);
    color:var(--text);line-height:1.7;font-size:15px;-webkit-font-smoothing:antialiased}
  .wrap{max-width:1040px;margin:0 auto;padding:0 24px 96px}
  .top{padding:56px 0 8px}
  .kicker{font-size:12.5px;letter-spacing:2.2px;text-transform:uppercase;color:var(--primary-d);font-weight:700;margin-bottom:10px}
  h1{font-size:clamp(28px,4.2vw,40px);font-weight:800;letter-spacing:-.6px;line-height:1.24;margin-bottom:12px}
  h1 .hl{background:linear-gradient(120deg,#0EA5A4,#22C55E 70%);-webkit-background-clip:text;background-clip:text;color:transparent}
  .lede{color:#334155;max-width:740px;font-size:16px}
  .meta{display:flex;flex-wrap:wrap;gap:9px;margin-top:22px}
  .tag{font-size:12.5px;padding:5px 13px;border-radius:999px;background:#fff;border:1px solid var(--line);color:#334155}
  .tag.on{background:linear-gradient(135deg,#0EA5A4,#14B8A6);color:#fff;border-color:transparent}
  section{margin-top:62px}
  .sec-head{display:flex;align-items:center;gap:12px;margin-bottom:6px}
  .sec-num{font-size:11.5px;font-weight:800;letter-spacing:1px;color:#fff;
    background:linear-gradient(135deg,#0EA5A4,#14B8A6);padding:4px 10px;border-radius:8px}
  h2{font-size:23px;font-weight:800;letter-spacing:-.3px}
  .sec-desc{color:var(--muted);font-size:14px;margin:6px 0 22px;max-width:820px}
  h3{font-size:15.5px;font-weight:700;margin:26px 0 12px;color:#334155}
  .panel{background:var(--surface);border:1px solid var(--line);border-radius:16px;padding:30px;
    box-shadow:var(--shadow);display:flex;align-items:center;justify-content:center}
  .panel.tint{background:linear-gradient(160deg,#EAF3F2,#F6FAFF)}
  .panel.dark{background:linear-gradient(160deg,#141C26,#070A0F);border-color:#1E2836}
  .grid2{display:grid;grid-template-columns:1fr 1fr;gap:16px}
  .grid4{display:grid;grid-template-columns:repeat(4,1fr);gap:16px}
  @media(max-width:880px){.grid2,.grid4{grid-template-columns:1fr 1fr}}
  @media(max-width:560px){.grid2,.grid4{grid-template-columns:1fr}}
  .vcard{background:#fff;border:1px solid var(--line);border-radius:14px;box-shadow:var(--shadow);overflow:hidden}
  .vcard .stage{height:190px;display:flex;align-items:center;justify-content:center;background:#FBFDFD}
  .vcard .stage.dark{background:linear-gradient(160deg,#141C26,#070A0F)}
  .vcard .cap{padding:12px 15px;border-top:1px solid var(--line-2);font-size:12.5px}
  .vcard .cap b{display:block;font-size:13.5px;font-weight:700}
  .vcard .cap code{font-size:11.5px;color:#94A3B8;font-family:ui-monospace,Consolas,monospace}
  .vcard .cap .use{color:var(--primary-d);font-size:12px;font-weight:600;margin-top:3px;display:block}
  .mk{width:132px;height:132px;display:block}
  .lk{width:100%;max-width:600px;height:auto;display:block}
  .lkv{width:100%;max-width:320px;height:auto;display:block}
  .icons{display:flex;align-items:flex-end;gap:26px;flex-wrap:wrap}
  .icon-demo{text-align:center}
  .icon-demo .lab{font-size:11.5px;color:var(--muted);margin-top:8px}
  .a128{width:128px;height:128px;border-radius:29px;box-shadow:0 12px 30px rgba(15,23,42,.18)}
  .a64{width:64px;height:64px;border-radius:15px;box-shadow:0 8px 18px rgba(15,23,42,.16)}
  .a32{width:32px;height:32px;border-radius:8px;box-shadow:0 4px 10px rgba(15,23,42,.16)}
  .a16{width:16px;height:16px;border-radius:4px}
  .swatches{display:grid;grid-template-columns:repeat(6,1fr);gap:12px}
  @media(max-width:880px){.swatches{grid-template-columns:repeat(3,1fr)}}
  .sw{border-radius:12px;overflow:hidden;border:1px solid var(--line);background:#fff}
  .sw .c{height:62px}
  .sw .l{padding:8px 10px;font-size:11.5px;color:#334155;font-weight:700;line-height:1.45}
  .sw .l code{display:block;color:#94A3B8;font-weight:500;font-size:10.5px}
  .rules{display:grid;grid-template-columns:repeat(4,1fr);gap:16px}
  @media(max-width:880px){.rules{grid-template-columns:1fr 1fr}}
  .rule{background:#fff;border:1px solid var(--line);border-radius:14px;overflow:hidden;box-shadow:var(--shadow)}
  .rule .stage{height:120px;display:flex;align-items:center;justify-content:center;background:#FBFDFD}
  .rule .cap{padding:10px 13px;font-size:12px;color:#B91C1C;font-weight:700;border-top:1px solid var(--line-2)}
  table{width:100%;border-collapse:separate;border-spacing:0;font-size:13.5px;margin:6px 0 14px;
    background:#fff;border:1px solid var(--line);border-radius:12px;overflow:hidden}
  th{background:var(--surface-2);text-align:left;padding:11px 14px;font-weight:700;border-bottom:1px solid var(--line);font-size:13px}
  td{padding:11px 14px;border-bottom:1px solid var(--line-2);color:#334155;vertical-align:top}
  tr:last-child td{border-bottom:none}
  code{font-family:ui-monospace,Consolas,monospace;font-size:12.5px;background:#F1F5F5;padding:1px 6px;border-radius:5px;color:#0F766E}
  .callout{border-left:3px solid var(--primary);background:linear-gradient(90deg,#F0FDFA,#FFFFFF);
    padding:14px 18px;border-radius:0 12px 12px 0;margin:16px 0;font-size:14px;color:#134E4A}
  .footer{margin-top:64px;padding-top:22px;border-top:1px solid var(--line);
    display:flex;justify-content:space-between;flex-wrap:wrap;gap:12px;font-size:12.5px;color:var(--muted)}
</style>
</head>
<body>
__SYMDEF__
<div class="wrap">
  <header class="top">
    <div class="kicker">Brand Identity · v1.0</div>
    <h1>老六清净 <span class="hl">old666</span> 品牌标志规范</h1>
    <p class="lede">图形标矢量化自商标文件 <code>assets/logo/source/reloadbug-flat.svg</code>（黑 + 赤红扁平标），
      <strong>形状完全保留</strong>。原始描摹件与扁平件均归档于 <code>assets/logo/source/</code>。</p>
    <div class="meta">
      <span class="tag on">矢量化自商标</span>
      <span class="tag">925 KB → 8.8 KB</span>
      <span class="tag">透明背景</span>
      <span class="tag">2 色 + 单色</span>
      <span class="tag">含深色背景版</span>
    </div>
  </header>

  <section>
    <div class="sec-head"><span class="sec-num">01</span><h2>主标志</h2></div>
    <p class="sec-desc">横版组合标：图形标在左，中文名与英文标识在右。用于启动页、关于页、官网首屏、文档页眉。</p>
    <div class="panel tint" style="padding:44px 30px"><svg class="lk"><use href="#lkH"/></svg></div>
  </section>

  <section>
    <div class="sec-head"><span class="sec-num">02</span><h2>图形标 · 五种形态</h2></div>
    <p class="sec-desc">形状完全一致，按背景与尺寸选择配色。</p>
    <div class="grid4">
      <div class="vcard"><div class="stage"><svg class="mk"><use href="#mark"/></svg></div>
        <div class="cap"><b>主用 · 黑 + 赤红</b><code>mark.svg</code><span class="use">浅色背景 · 默认</span></div></div>
      <div class="vcard"><div class="stage dark"><svg class="mk"><use href="#markDark"/></svg></div>
        <div class="cap"><b>深色背景版</b><code>mark-dark.svg</code><span class="use">深色界面 / 深色底</span></div></div>
      <div class="vcard"><div class="stage"><svg class="mk"><use href="#monoDark"/></svg></div>
        <div class="cap"><b>单色 · 墨黑</b><code>mark-mono-dark.svg</code><span class="use">印章 / 水印 / 单色印刷</span></div></div>
      <div class="vcard"><div class="stage dark"><svg class="mk"><use href="#monoLight"/></svg></div>
        <div class="cap"><b>单色 · 纯白</b><code>mark-mono-light.svg</code><span class="use">深色底单色场景</span></div></div>
    </div>
    <h3>光泽版（可选 · 大尺寸展示）</h3>
    <div class="panel tint" style="padding:34px">
      <svg style="width:230px;height:230px;display:block"><use href="#markGlossy"/></svg>
      <div style="margin-left:30px;font-size:13.5px;color:#475569;max-width:360px">
        源自商标原始渲染图，保留高光与质感。<strong>仅用于大尺寸场合</strong>（启动页、官网首屏、宣传物料），
        界面内嵌请使用主用扁平版。
      </div>
    </div>
  </section>

  <section>
    <div class="sec-head"><span class="sec-num">03</span><h2>组合标</h2></div>
    <p class="sec-desc">横版用于宽幅场景，竖版用于方形场景（启动闪屏、安装向导）。</p>
    <div class="grid2">
      <div class="panel tint" style="padding:34px 24px"><svg class="lk"><use href="#lkH"/></svg></div>
      <div class="panel tint" style="padding:30px 24px"><svg class="lkv"><use href="#lkV"/></svg></div>
    </div>
  </section>

  <section>
    <div class="sec-head"><span class="sec-num">04</span><h2>应用图标</h2></div>
    <p class="sec-desc">深色底版本为主（延续商标的黑），桌面与任务栏对比度最好；浅色底用于浅色主题系统。</p>
    <div class="panel tint" style="padding:40px 30px">
      <div class="icons">
        <div class="icon-demo"><svg class="a128"><use href="#appDark"/></svg><div class="lab">128 px</div></div>
        <div class="icon-demo"><svg class="a64"><use href="#appDark"/></svg><div class="lab">64 px</div></div>
        <div class="icon-demo"><svg class="a32"><use href="#appDark"/></svg><div class="lab">32 px</div></div>
        <div class="icon-demo"><svg class="a16"><use href="#appDark"/></svg><div class="lab">16 px</div></div>
        <div style="width:1px;height:96px;background:var(--line);margin:0 6px"></div>
        <div class="icon-demo"><svg class="a128"><use href="#appLight"/></svg><div class="lab">128 px · 浅色</div></div>
        <div class="icon-demo"><svg class="a64"><use href="#appLight"/></svg><div class="lab">64 px</div></div>
        <div class="icon-demo"><svg class="a32"><use href="#appLight"/></svg><div class="lab">32 px</div></div>
      </div>
    </div>
  </section>

  <section>
    <div class="sec-head"><span class="sec-num">05</span><h2>品牌配色</h2></div>
    <p class="sec-desc">标志用色与界面用色分工：<strong>标志黑红 · 界面青绿</strong>。</p>
    <div class="swatches">
      <div class="sw"><div class="c" style="background:#0E1116"></div><div class="l">墨黑<code>#0E1116 · 标志主色</code></div></div>
      <div class="sw"><div class="c" style="background:linear-gradient(135deg,#FF7A45,#F5340F 50%,#C41C05)"></div><div class="l">赤红<code>#F5340F · 标志点睛</code></div></div>
      <div class="sw"><div class="c" style="background:#0EA5A4"></div><div class="l">主青绿<code>#0EA5A4 · 界面主色</code></div></div>
      <div class="sw"><div class="c" style="background:#22C55E"></div><div class="l">强调绿<code>#22C55E · 渐变</code></div></div>
      <div class="sw"><div class="c" style="background:#F4F8F8;border-bottom:1px solid #E2E8F0"></div><div class="l">界面底<code>#F4F8F8</code></div></div>
      <div class="sw"><div class="c" style="background:#FFFFFF;border-bottom:1px solid #E2E8F0"></div><div class="l">卡片白<code>#FFFFFF</code></div></div>
    </div>
  </section>

  <section>
    <div class="sec-head"><span class="sec-num">06</span><h2>使用规范</h2></div>
    <h3>安全边距</h3>
    <div class="panel" style="padding:34px">
      <div style="border:1.5px dashed #CBD5E1;border-radius:12px;padding:32px;display:inline-block">
        <svg style="width:130px;height:130px;display:block"><use href="#mark"/></svg>
      </div>
      <div style="margin-left:34px;font-size:13.5px;color:#475569;max-width:320px">
        四周留白 <strong>≥ X</strong>，X 取耳部圆球直径。任何文字、边框、其他元素不得进入该区域。
      </div>
    </div>
    <h3>最小尺寸</h3>
    <div class="panel" style="padding:30px;gap:40px;flex-wrap:wrap">
      <div style="text-align:center"><svg style="width:48px;height:48px;display:block"><use href="#mark"/></svg><div style="font-size:11.5px;color:#64748B;margin-top:6px">48 px</div></div>
      <div style="text-align:center"><svg style="width:32px;height:32px;display:block"><use href="#mark"/></svg><div style="font-size:11.5px;color:#64748B;margin-top:6px">32 px</div></div>
      <div style="text-align:center"><svg style="width:24px;height:24px;display:block"><use href="#monoDark"/></svg><div style="font-size:11.5px;color:#64748B;margin-top:6px">24 px 改用单色</div></div>
    </div>
    <h3>禁止用法</h3>
    <div class="rules">
      <div class="rule"><div class="stage"><svg style="width:100px;height:100px;display:block"><use href="#mark"/></svg></div><div class="cap">不要拉伸或压扁</div></div>
      <div class="rule"><div class="stage"><svg style="width:100px;height:100px;display:block;filter:hue-rotate(200deg)"><use href="#mark"/></svg></div><div class="cap">不要随意改眼睛颜色</div></div>
      <div class="rule"><div class="stage"><svg style="width:100px;height:100px;display:block;transform:rotate(-18deg)"><use href="#mark"/></svg></div><div class="cap">不要旋转或倾斜</div></div>
      <div class="rule"><div class="stage"><svg style="width:100px;height:100px;display:block;filter:drop-shadow(0 0 8px #0EA5A4)"><use href="#mark"/></svg></div><div class="cap">不要加投影 / 发光</div></div>
    </div>
  </section>

  <section>
    <div class="sec-head"><span class="sec-num">07</span><h2>文件清单</h2></div>
    <table>
      <thead><tr><th style="width:30%">文件</th><th style="width:52%">用途</th><th style="width:18%">体积</th></tr></thead>
      <tbody>
__ROWS__
      </tbody>
    </table>
    <div class="callout">
      <strong>关于原始文件：</strong>商标原始矢量件（黑 + 赤红扁平标，内嵌位图 + 分层引用）
      归档于 <code>assets/logo/source/reloadbug-flat.svg</code>；早期自动描摹件
      <code>assets/logo/source/reloadbug.svg</code>（925 KB / 1319 路径）一并保留备查。
      产品用资源由 <code>tools/vectorize-logo.py</code> 自动生成，可随时重跑复现。
    </div>
  </section>

  <div class="footer">
    <span>老六清净 old666 · 品牌标志规范 v1.0</span>
    <span>商标形状完全保留 · 仅做矢量化优化与背景适配</span>
  </div>
</div>
</body>
</html>
'''

HTML = HTML.replace('__SYMDEF__', SYMDEF).replace('__ROWS__', size_rows)
open(OUT, 'w', encoding='utf-8').write(HTML)
print('生成 %s  %.1f KB' % (OUT, os.path.getsize(OUT) / 1024))
