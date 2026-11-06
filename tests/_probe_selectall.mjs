/**
 * 临时探针：复现「全选不好使」。
 * 用真机 fixture 驱动 src/index.html，勾选表头全选框，观察行勾选、按钮态与 picked 集合。
 * 只读，不写任何系统状态。
 */
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';

function loadChromium() {
  const req = createRequire(import.meta.url);
  const candidates = [
    process.env.PLAYWRIGHT_DIR,
    join(dirname(fileURLToPath(import.meta.url)), '..', 'node_modules'),
    'C:/Users/Administrator/.workbuddy-ai/binaries/node/workspace/node_modules',
  ].filter(Boolean);
  let entry = null;
  for (const dir of candidates) {
    try { entry = req.resolve('playwright', { paths: [dir] }); break; } catch { /* 下一个 */ }
  }
  if (!entry) throw new Error('找不到 playwright');
  return import(pathToFileURL(entry).href).then((m) => m.chromium || m.default.chromium);
}
function findChromium() {
  const base = process.env.LOCALAPPDATA ? join(process.env.LOCALAPPDATA, 'ms-playwright') : null;
  const guesses = base ? [
    join(base, 'chromium_headless_shell-1243', 'chrome-headless-shell-win64', 'chrome-headless-shell.exe'),
    join(base, 'chromium_headless_shell-1248', 'chrome-headless-shell-win64', 'chrome-headless-shell.exe'),
    join(base, 'chromium-1243', 'chrome-win64', 'chrome.exe'),
  ] : [];
  return guesses.find((p) => existsSync(p)) || undefined;
}

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(ROOT, 'src');
const FIX = join(ROOT, 'tests', '_out', 'fixtures');

const boot = JSON.parse(readFileSync(join(FIX, 'boot.json'), 'utf8'));
const fastRaw = JSON.parse(readFileSync(join(FIX, 'fast.json'), 'utf8'));
const deep = JSON.parse(readFileSync(join(FIX, 'deep.json'), 'utf8'));

const chromium = await loadChromium();
const exe = findChromium();
const browser = await chromium.launch(exe ? { executablePath: exe } : {});
/* 用 Tauri 的默认窗口尺寸，而不是宽视口 —— 只有真实尺寸下才能暴露
   「点了没反应」这类命中问题。 */
const VP = { width: 1180, height: 780 };
const page = await browser.newPage({ viewport: VP });

const errors = [];
page.on('pageerror', (e) => errors.push('pageerror: ' + String(e)));
page.on('console', (m) => { if (m.type() === 'error') errors.push('console: ' + m.text()); });

await page.addInitScript(({ boot, fast, deep }) => {
  const ok = (data) => ({ ok: true, data, error: null });
  const err = (msg) => ({ ok: false, data: null, error: msg });
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        switch (cmd) {
          case 'scan_processes':
            return ok({ processes: [], metrics: { cpu_percent: 0, mem_used: 0, mem_total: 1, disk_percent: 0, disk_read_bps: 0, disk_write_bps: 0, net_rx_bps: 0, net_tx_bps: 0, process_count: 0, thread_count: 0, uptime_secs: 1 }, stats: { process_count: 0, enumerate_ms: 0, detail_ms: 0, total_ms: 0 } });
          case 'env_info':
            return ok({ version: '0.1.0', cpu_count: 16, pdh_healthy: true, portable: true, data_dir: 'D:\\old666\\data', admin: true, blacklist_coverage: [237, 14, 47, 46, 6] });
          case 'whitelist_list': return ok({ names: [], paths: [] });
          case 'snapshot_list': return ok([]);
          case 'boot_timeline': return ok(boot);
          case 'startup_scan': return ok(fast);
          case 'startup_verify': {
            const only = new Set(args.ids || []);
            const items = deep.items.map((i) =>
              only.size === 0 || only.has(i.id) ? i : { ...i, signature_pending: true });
            return ok({ ...deep, items, pending_signatures: items.filter((i) => i.signature_pending).length });
          }
          case 'startup_batches': return ok([]);
          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
}, { boot, fast: fastRaw, deep });

await page.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/index.html');
await page.waitForTimeout(400);
await page.click('.nav-item[data-view="boot"]');
await page.waitForSelector('#viewBoot:not([hidden])');
await page.waitForFunction(() => document.querySelectorAll('#startupBody tr').length > 0);
await page.waitForTimeout(800);   // 等补签回流

const snap = () => page.evaluate(() => {
  const all = document.querySelector('#chkStartupAll');
  const boxes = [...document.querySelectorAll('#startupBody input[data-id]')];
  return {
    all: { checked: all.checked, indeterminate: all.indeterminate, disabled: all.disabled },
    rows: document.querySelectorAll('#startupBody tr').length,
    boxes: boxes.length,
    boxDisabled: boxes.filter((c) => c.disabled).length,
    boxChecked: boxes.filter((c) => c.checked).length,
    btnDisable: document.querySelector('#btnStartupDisable').textContent.trim(),
    btnDisableDisabled: document.querySelector('#btnStartupDisable').disabled,
    btnDelay: document.querySelector('#btnStartupDelay').textContent.trim(),
  };
});

console.log('--- fixture 统计 ---');
const byRisk = {};
for (const i of deep.items) byRisk[i.risk] = (byRisk[i.risk] || 0) + 1;
console.log('总数', deep.items.length, JSON.stringify(byRisk));
console.log('enabled 且 can_disable:',
  deep.items.filter((i) => i.enabled && i.can_disable).length);
console.log('可安全禁用(safe+enabled+can_disable):',
  deep.items.filter((i) => i.risk === 'safe' && i.enabled && i.can_disable).length);

console.log('\n--- 初始 ---');
console.log(JSON.stringify(await snap(), null, 1));

console.log('\n--- 勾选表头全选 ---');
await page.evaluate(() => {
  const all = document.querySelector('#chkStartupAll');
  all.checked = true;
  all.dispatchEvent(new Event('change', { bubbles: true }));
});
await page.waitForTimeout(300);
console.log(JSON.stringify(await snap(), null, 1));

console.log('\n--- 取消全选 ---');
await page.evaluate(() => {
  const all = document.querySelector('#chkStartupAll');
  all.checked = false;
  all.dispatchEvent(new Event('change', { bubbles: true }));
});
await page.waitForTimeout(300);
console.log(JSON.stringify(await snap(), null, 1));

/* ---- 命中诊断：全选框在真实窗口尺寸下能不能点到 ---- */
async function hitProbe(tag, scrollY) {
  if (scrollY != null) {
    await page.evaluate((y) => window.scrollTo(0, y), scrollY);
    await page.waitForTimeout(150);
  }
  const r = await page.evaluate((tag) => {
    const el = document.querySelector('#chkStartupAll');
    const rect = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    const cx = rect.left + rect.width / 2, cy = rect.top + rect.height / 2;
    const pts = [
      ['center', cx, cy],
      ['left2', rect.left + 2, cy],
      ['top2', cx, rect.top + 2],
      ['right2', rect.right - 2, cy],
    ].map(([n, x, y]) => {
      const h = document.elementFromPoint(x, y);
      const cls = h && typeof h.className === 'string' && h.className
        ? '.' + h.className.trim().split(/\s+/)[0] : '';
      return {
        p: n, x: Math.round(x), y: Math.round(y),
        hit: h ? h.tagName + (h.id ? '#' + h.id : '') + cls : null,
        ok: !!h && (h === el || !!h.closest('label.chk')),
      };
    });
    return {
      tag,
      scrollY: Math.round(window.scrollY),
      docH: document.documentElement.scrollHeight,
      vpH: window.innerHeight,
      elRect: [rect.x | 0, rect.y | 0, rect.width | 0, rect.height | 0],
      inViewport: rect.top >= 0 && rect.bottom <= window.innerHeight,
      opacity: cs.opacity, visibility: cs.visibility, pointerEvents: cs.pointerEvents,
      pts,
      allHit: pts.every((p) => p.ok),
    };
  }, tag);
  console.log(JSON.stringify(r));
  return r;
}

console.log('\n--- 命中诊断（窗口 1180x780）---');
await hitProbe('页面顶部', 0);
await page.evaluate(() => window.scrollTo(0, Math.round(document.documentElement.scrollHeight / 2)));
await hitProbe('页面中部');
await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
await hitProbe('页面底部');
await page.evaluate(() => window.scrollTo(0, 0));
await page.waitForTimeout(150);

/* 真实点击（playwright 会先把元素滚进视口 —— 真实用户做不到这一点） */
console.log('\n--- 真实点击 #chkStartupAll ---');
try {
  await page.click('#chkStartupAll', { timeout: 2500 });
  await page.waitForTimeout(250);
  const after = await snap();
  console.log('真实点击后 boxChecked =', after.boxChecked, ' btn =', after.btnDisable);
} catch (e) {
  console.log('真实点击失败：', String(e).split('\n')[0]);
}
try {
  await page.click('#viewBoot .startup-filter label.chk', { timeout: 2500 });
  await page.waitForTimeout(250);
  const after = await snap();
  console.log('点 label 后 boxChecked =', after.boxChecked, ' btn =', after.btnDisable);
} catch (e) {
  console.log('点 label 失败：', String(e).split('\n')[0]);
}

/* ---- 已禁用项的可操作性：能否单独还原 ---- */
console.log('\n--- 已禁用项的可操作性 ---');
const dis = deep.items.filter((i) => !i.enabled);
console.log('已禁用条目数', dis.length);
const disRows = await page.evaluate(() => {
  const out = { rows: 0, disabledCb: 0, withInlineBtn: 0, samples: [] };
  for (const tr of document.querySelectorAll('#startupBody tr')) {
    const txt = tr.querySelector('.startup-env');
    if (!txt || txt.textContent.trim() !== '已禁用') continue;
    out.rows++;
    const cb = tr.querySelector('input[data-id]');
    if (cb && cb.disabled) out.disabledCb++;
    if (tr.querySelector('button[data-act]')) out.withInlineBtn++;
    if (out.samples.length < 3) {
      out.samples.push({
        name: tr.querySelector('.startup-name')?.textContent,
        cbDisabled: cb ? cb.disabled : null,
        buttons: [...tr.querySelectorAll('button')].map((b) => b.textContent.trim()),
      });
    }
  }
  return out;
});
console.log(JSON.stringify(disRows, null, 1));

/* ---- 场景：手动勾 3 行，表头是否半选 ---- */
console.log('\n--- 手动勾 3 行 → 表头半选 ---');
await page.evaluate(() => {
  const all = document.querySelector('#chkStartupAll');
  if (all.checked) { all.checked = false; all.dispatchEvent(new Event('change', { bubbles: true })); }
  const boxes = [...document.querySelectorAll('#startupBody input[data-id]')].filter((c) => !c.disabled).slice(0, 3);
  for (const b of boxes) { b.checked = true; b.dispatchEvent(new Event('change', { bubbles: true })); }
});
await page.waitForTimeout(250);
{
  const s = await snap();
  console.log('勾 3 行后：表头 checked=', s.all.checked, 'indeterminate=', s.all.indeterminate, '行勾=', s.boxChecked, 'btn=', s.btnDisable);
}

/* ---- 场景：全选 → 打开禁用确认弹窗，清单条数是否 = 选中数 ---- */
console.log('\n--- 全选 → 禁用确认弹窗 ---');
await page.evaluate(() => {
  const all = document.querySelector('#chkStartupAll');
  all.checked = true; all.dispatchEvent(new Event('change', { bubbles: true }));
});
await page.waitForTimeout(250);
await page.click('#btnStartupDisable');
await page.waitForTimeout(500);
{
  const modal = await page.evaluate(() => ({
    visible: !document.querySelector('#confirmMask').hidden,
    lead: document.querySelector('#confirmLead')?.textContent,
    items: document.querySelectorAll('#confirmList li').length,
    title: document.querySelector('#confirmTitle')?.textContent,
  }));
  console.log(JSON.stringify(modal, null, 1));
}
await page.evaluate(() => { document.querySelector('#confirmMask').hidden = true; });

/* ---- 场景：筛选到「服务」后再全选 ---- */
console.log('\n--- 筛选=服务 后全选 ---');
await page.click('#startupFilters .chip[data-src="service"]');
await page.waitForTimeout(200);
await page.evaluate(() => {
  const all = document.querySelector('#chkStartupAll');
  if (!all.checked) { all.checked = true; all.dispatchEvent(new Event('change', { bubbles: true })); }
});
await page.waitForTimeout(250);
{
  const s = await snap();
  console.log('服务视图：行=', s.rows, '行勾=', s.boxChecked, '表头checked=', s.all.checked, 'btn=', s.btnDisable);
}
/* 切回全部：选中集合是否被带过来（跨视图残留） */
await page.click('#startupFilters .chip[data-src="all"]');
await page.waitForTimeout(200);
{
  const s = await snap();
  console.log('切回全部：行=', s.rows, '行勾=', s.boxChecked, '表头checked=', s.all.checked,
    'indeterminate=', s.all.indeterminate, 'btn=', s.btnDisable);
}

console.log('\n--- 页面错误 ---');
console.log(errors.length ? errors.join('\n') : '（无）');

await browser.close();
