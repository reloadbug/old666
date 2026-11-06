/**
 * 老六清净 old666 · 开机加速页 无头渲染验证（P2-5）
 * ------------------------------------------------------------
 * 为什么要这个脚本：
 *   真实 GUI 依赖 WebView2 + Tauri 运行时，CI / 沙箱里起不来；而这一页
 *   恰好是**数据合同最容易错**的地方——后端把 148 个启动项、8 条开机记录
 *   按严格的 JSON 形状交给前端，字段名差一个字母整页就白屏。
 *
 * 做法：
 *   1. file:// 直接加载 src/index.html（不需要起服务器）；
 *   2. 在页面脚本执行前注入 window.__TAURI__，invoke 用**真机导出的
 *      真实后端 payload**（boot.json / fast.json / deep.json）逐个命令应答；
 *   3. 点「开机加速」导航，断言分段条、图例、趋势图、归因列表、启动项
 *      表格、两段式补签、禁用确认弹窗全部按真实数据渲染；
 *   4. 全页截图留档。
 *
 * 数据来源（真机、只读导出，缺了就由本脚本自动跑 CLI 生成到 tests/_out/fixtures/）：
 *   old666-poc --boot --json             → boot.json
 *   old666-poc --startup --fast --json   → fast.json（未补签）
 *   old666-poc --startup --json          → deep.json（已补签）
 *
 * 运行方式：
 *   node tests/frontend_boot.mjs
 *   （需要 playwright；本仓用 --no-save 装在共享 workspace 里）
 */

import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';

/** playwright 可能装在共享 workspace，也可能是本地 node_modules —— 按顺序找。 */
function loadChromium() {
  const req = createRequire(import.meta.url);
  const candidates = [
    process.env.PLAYWRIGHT_DIR,
    join(dirname(fileURLToPath(import.meta.url)), '..', 'node_modules'),
    'C:/Users/Administrator/.workbuddy-ai/binaries/node/workspace/node_modules',
  ].filter(Boolean);
  let entry = null;
  for (const dir of candidates) {
    try { entry = req.resolve('playwright', { paths: [dir] }); break; } catch { /* 换个目录再试 */ }
  }
  if (!entry) throw new Error('找不到 playwright，请先 npm install playwright');
  return import(pathToFileURL(entry).href).then((m) => m.chromium || m.default.chromium);
}

/** 浏览器二进制：playwright 版本与已下载的 chromium 版本对不上很常见，
 *  这里直接指定本机已存在的那个，避免 `npx playwright install` 走网络。 */
function findChromium() {
  const base = process.env.LOCALAPPDATA
    ? join(process.env.LOCALAPPDATA, 'ms-playwright') : null;
  const guesses = base ? [
    join(base, 'chromium_headless_shell-1243', 'chrome-headless-shell-win64', 'chrome-headless-shell.exe'),
    join(base, 'chromium_headless_shell-1248', 'chrome-headless-shell-win64', 'chrome-headless-shell.exe'),
    join(base, 'chromium-1243', 'chrome-win64', 'chrome.exe'),
  ] : [];
  return guesses.find((p) => existsSync(p)) || undefined;
}

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(ROOT, 'src');
const OUT = join(ROOT, 'tests', '_out');
const FIX = join(OUT, 'fixtures');

/** 三份真机 fixture（不是手写假数据）。本环境的沙箱不允许 node 里 spawn 进程，
 *  所以导出交给 `bash tests/gen-fixtures.sh`；缺 fixture 时这里给出明确指引，
 *  而不是静默退回假数据 —— 那会测出一个不存在的契约。 */
function loadFixtures() {
  const names = ['boot.json', 'fast.json', 'deep.json'];
  const missing = names.filter((f) => !existsSync(join(FIX, f)));
  if (missing.length) {
    throw new Error(
      `缺少 fixture：${missing.join('、')}\n` +
      '  请先导出（真机只读，几秒钟）：\n' +
      '    bash tests/gen-fixtures.sh');
  }
  const read = (f) => JSON.parse(readFileSync(join(FIX, f), 'utf8'));
  return { boot: read('boot.json'), fastRaw: read('fast.json'), deep: read('deep.json') };
}

const { boot, fastRaw, deep } = loadFixtures();

const impactById = new Map(
  deep.items.filter((i) => i.impact_ms).map((i) => [i.id, i.impact_ms]));
const fast = {
  ...fastRaw,
  items: fastRaw.items.map((i) =>
    impactById.has(i.id) ? { ...i, impact_ms: impactById.get(i.id) } : i),
};


const chromium = await loadChromium();
const exe = findChromium();

const results = [];
function check(name, pass, detail = '') {
  results.push({ name, pass, detail });
  console.log(`${pass ? '  ✓' : '  ✗'} ${name}${detail ? '  — ' + detail : ''}`);
  return pass;
}

mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch(exe ? { executablePath: exe } : {});
const page = await browser.newPage({ viewport: { width: 1280, height: 1500 } });

const pageErrors = [];
page.on('pageerror', (e) => pageErrors.push(String(e)));
page.on('console', (m) => { if (m.type() === 'error') pageErrors.push('console: ' + m.text()); });

/** 在页面脚本执行前接入 Tauri 的 invoke。
 *  注意：addInitScript 的 data 参数会被 JSON 序列化，**函数过不去**，
 *  所以应答逻辑必须写在 init 脚本体内，只把真机 payload 当数据传进来。 */
await page.addInitScript(({ boot, fast, deep }) => {
  const ok = (data) => ({ ok: true, data, error: null });
  const err = (msg) => ({ ok: false, data: null, error: msg });
  let applied = [];

  const byId = new Map(deep.items.map((i) => [i.id, i]));

  /* ---- 模拟「本次会话中被改动的状态」 ------------------------------------
     真实后端把「白/黑名单」「用户知识库」「被我们禁用的条目」都落成外置 JSON，
     下一次 scan 会如实反映。mock 必须在内存里扮演同一件事，否则
     「加黑 → 立即禁用 → 重扫后显示已禁用」这条链路根本走不通。 */
  const mockState = {
    white: new Set(),
    black: new Set(),
    off: new Set(),      // 被禁用的 item id
    on: new Set(),       // 被显式启用的 item id（覆盖 fixture 里本来就 disabled 的那些）
    protectedIds: new Set(), // 模拟「服务受保护、拿不到修改权限」的条目
    userKb: [],          // 用户知识库条目
    userPath: 'D:\\old666\\data\\knowledge\\startup.json',
    listsPath: 'D:\\old666\\data\\startup_lists.json',
  };
  const kbHits = [];     // 记录 knowledge_save / remove 的调用，供断言

  /** 与 boot.js `exeName` 同一套启发式（先找 .exe，再回溯到最后一个分隔符）。 */
  const exeNameOf = (cmd) => {
    const s = String(cmd || '').toLowerCase();
    const i = s.indexOf('.exe');
    if (i < 0) return '';
    const head = s.slice(0, i);
    let start = 0;
    for (const ch of ['\\', '/', '"', '|', ',']) {
      const p = head.lastIndexOf(ch);
      if (p >= 0) start = Math.max(start, p + 1);
    }
    return head.slice(start).trim();
  };
  const nameOf = (it) => exeNameOf(it.command) || it.name || '';
  const normName = (s) => String(s || '').trim().toLowerCase().replace(/\.exe$/, '');

  /** 把内存状态叠加到扫描结果上 —— 等价于后端下一次 scan 的产出。 */
  const decorate = (list) => list.map((it) => {
    const off = mockState.off.has(it.id);
    const on = mockState.on.has(it.id);
    const n = normName(nameOf(it));
    return {
      ...it,
      enabled: on ? true : (off ? false : it.enabled),
      disabled_by_us: off,
      batch_id: off ? 'sb-65f0a1b2-0001-debug' : it.batch_id,
      in_whitelist: mockState.white.has(n),
      in_blacklist: mockState.black.has(n),
      knowledge: it.knowledge,
    };
  });

  const listsPayload = () => ({
    whitelist: [...mockState.white],
    blacklist: [...mockState.black],
    path: mockState.listsPath,
  });
  const kbPayload = () => ({
    builtin: 173,
    user: mockState.userKb.length,
    user_path: mockState.userPath,
    user_entries: mockState.userKb.map((e) => ({ key: e.key, title: e.title, advice: e.advice })),
  });

  const undoOf = (it) => it.source === 'service'
    ? JSON.stringify({ key: it.location.split('!').pop(), start: 2 })
    : JSON.stringify({ reg: it.location, value: null });

  const planChanges = (want, action = 'disable') => want.map((id) => {
    const it = byId.get(id);
    if (!it) return { item_id: id, name: '(已不存在)', source: 'run_key', action, undo: 'null', ok: false, error: '条目已不存在' };
    // 权限预检失败（真实后端在演练阶段就 probe，见 startup_ctl::probe_writable）
    if (mockState.protectedIds.has(id)) {
      return {
        item_id: id, name: it.name, source: it.source, action,
        undo: '', ok: false,
        error: `「${it.name}」被系统拒绝修改：该服务受保护（常见于安全软件自保护，或只允许 SYSTEM 修改），本程序不会绕过它`,
      };
    }
    // 延迟启动只有自动启动的服务支持 —— 与真实后端行为一致
    if (action !== 'disable' && it.source !== 'service') {
      return {
        item_id: id, name: it.name, source: it.source, action,
        undo: '', ok: false,
        error: '该来源不支持延迟启动（仅自动启动的服务支持）',
      };
    }
    // 「禁用」只挡「不建议禁用」的来源；黑名单里的条目后端会放行
    // （黑名单 = 用户明确表态「这个我要关，别再问我」）
    const blockedByRisk = it.risk === 'dangerous' && !it.can_disable;
    return {
      item_id: id, name: it.name, source: it.source,
      action,
      undo: action === 'disable' ? undoOf(it) : JSON.stringify({ key: it.id, delayed: action === 'delay' }),
      ok: !blockedByRisk,
      error: blockedByRisk ? '「不建议禁用」的条目已跳过' : null,
    };
  });

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        switch (cmd) {
          /* 进程页：给份最小可用数据，别让 init() 崩 */
          case 'scan_processes':
            return ok({
              processes: [],
              metrics: { cpu_percent: 0, mem_used: 0, mem_total: 1, disk_percent: 0, disk_read_bps: 0, disk_write_bps: 0, net_rx_bps: 0, net_tx_bps: 0, process_count: 0, thread_count: 0, uptime_secs: 1 },
              stats: { process_count: 0, enumerate_ms: 0, detail_ms: 0, total_ms: 0 },
            });
          case 'env_info':
            return ok({ version: '0.1.3', cpu_count: 16, pdh_healthy: true, portable: true, data_dir: 'D:\\old666\\data', admin: true, blacklist_coverage: [237, 14, 47, 46, 6] });
          case 'whitelist_list': return ok({ names: [], paths: [] });
          case 'snapshot_list': return ok([]);

          /* 开机加速 */
          case 'boot_timeline': return ok(boot);
          case 'startup_scan': return ok({ ...fast, items: decorate(fast.items) });
          case 'startup_verify': {
            /* 真实后端补签要读几百 MB（实测 ~0.8 s）。这里压到 400 ms：
               既要让「先出列表、再补签」的中间态可观测，又别拖慢测试。 */
            await new Promise((r) => setTimeout(r, 400));
            const only = new Set(args.ids || []);
            const base = decorate(deep.items).map((i) =>
              only.size === 0 || only.has(i.id) ? i : { ...i, signature_pending: true });
            return ok({ ...deep, items: base, pending_signatures: base.filter((i) => i.signature_pending).length });
          }
          case 'startup_batches':
            return ok(applied.length ? ['sb-65f0a1b2-0001-debug'] : []);
          case 'startup_plan': {
            const want = args.ids || [];
            if (!want.length) return err('没有找到对应的启动项');
            return ok({ batch_id: 'sb-plan', timestamp: 0, changes: planChanges(want, args.action || 'disable'), dry_run: true, elapsed_ms: 3 });
          }
          case 'startup_apply': {
            const want = args.ids || [];
            if (!want.length) return err('没有找到对应的启动项');
            const act = args.action || 'disable';
            applied.push(...want.map((id) => ({ id, action: act })));
            // 让「禁用/删除」把条目留在列表里并标记为已禁用 —— 正是用户要的
            // 「保留在列表，可一键启用」；「启用」则把它恢复。
            for (const id of want) {
              if (act === 'enable') { mockState.off.delete(id); mockState.on.add(id); }
              else { mockState.on.delete(id); mockState.off.add(id); }
            }
            return ok({ batch_id: 'sb-65f0a1b2-0001-debug', timestamp: 0, changes: planChanges(want, act).map((c) => ({ ...c, ok: true, error: null })), dry_run: false, elapsed_ms: 12 });
          }
          case 'startup_restore': {
            mockState.off.clear();
            mockState.on.clear();
            return ok({ ok: 1, fail: 0, errors: [] });
          }

          /* 知识库 */
          case 'knowledge_stats': return ok(kbPayload());
          case 'knowledge_save': {
            kbHits.push({ op: 'save', key: args.key, title: args.title, advice: args.advice });
            const i = mockState.userKb.findIndex((e) => e.key === args.key);
            const row = { key: args.key, title: args.title, vendor: args.vendor || '', desc: args.desc, advice: args.advice || '谨慎处理' };
            if (i >= 0) mockState.userKb[i] = row; else mockState.userKb.push(row);
            return ok(kbPayload());
          }
          case 'knowledge_remove': {
            kbHits.push({ op: 'remove', key: args.key });
            mockState.userKb = mockState.userKb.filter((e) => e.key !== args.key);
            return ok(kbPayload());
          }

          /* 启动项白名单 / 黑名单 */
          case 'startup_lists': return ok(listsPayload());
          case 'startup_list_add': {
            const n = normName(args.name);
            if (!n) return err('名称不能为空');
            if (args.kind === 'white') {
              mockState.black.delete(n);
              mockState.white.add(n);
              return ok({ lists: listsPayload(), disabled: 0, failed: [], dry_run: false });
            }
            mockState.white.delete(n);
            mockState.black.add(n);
            // 黑名单立即生效：把匹配到的、当前还启用的条目禁掉
            const hits = decorate(deep.items).filter((i) => normName(nameOf(i)) === n && i.enabled);
            for (const h of hits) mockState.off.add(h.id);
            return ok({ lists: listsPayload(), disabled: hits.length, failed: [], dry_run: false });
          }
          case 'startup_list_remove': {
            const n = normName(args.name);
            if (args.kind === 'white') mockState.white.delete(n);
            else mockState.black.delete(n);
            return ok(listsPayload());
          }

          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
  window.__TAURI_TEST__ = { applied, subs: [], mockState, kbHits };
  /* 记录副标题的每一次变化，用来断言两段式的先后顺序（不至于和渲染赛跑） */
  document.addEventListener('DOMContentLoaded', () => {
    const el = document.getElementById('startupSub');
    if (!el) return;
    new MutationObserver(() => {
      const t = el.textContent;
      const s = window.__TAURI_TEST__.subs;
      if (s[s.length - 1] !== t) s.push(t);
    }).observe(el, { childList: true, characterData: true, subtree: true });
  });
}, { boot, fast, deep });

await page.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/index.html');
await page.waitForTimeout(400);

console.log('\n== 1. 进入开机加速 ==');
await page.click('.nav-item[data-view="boot"]');
await page.waitForSelector('#viewBoot:not([hidden])');
await page.waitForFunction(() => document.querySelectorAll('#bootBar .seg').length > 0);
await page.waitForTimeout(300);   // 等补签回流

check('视图切到开机加速', await page.isVisible('#viewBoot'));
check('标题更新', (await page.textContent('#viewTitle')) === '开机加速');
/* [hidden] 曾经被 .btn{display:inline-flex} 之类盖掉，按钮们会一直挂在上层。
   这两条就是回归护栏：开机加速页不该出现刷新按钮，管理员模式不该出现提权按钮。 */
check('刷新按钮在开机加速页真的隐藏', await page.isHidden('#btnRefresh'));
check('管理员模式下提权按钮不出现', await page.isHidden('#btnElevate'));
check('设置抽屉没有常驻拦截点击', await page.isHidden('#drawer'));

console.log('\n== 2. 开机时间线 ==');
const cur = boot.records[0];
check('大数字 = 最近一次开机', (await page.textContent('#bootNow')).replace(/\s/g, '') ===
  (cur.boot_ms >= 1000 ? (cur.boot_ms / 1000).toFixed(1) + 's' : cur.boot_ms + 'ms'),
  await page.textContent('#bootNow'));

const segs = await page.$$eval('#bootBar .seg', (els) => els.map((e) => e.title));
check('分段数 = 阶段数', segs.length === cur.phases.length, `${segs.length} vs ${cur.phases.length}`);
const widths = await page.$$eval('#bootBar .seg', (els) => els.map((e) => parseFloat(e.style.width)));
check('分段宽度和 ≈ 100%', Math.abs(widths.reduce((a, b) => a + b, 0) - 100) < 0.5,
  widths.reduce((a, b) => a + b, 0).toFixed(2) + '%');
check('分段标注了阶段名', segs.every((t) => cur.phases.some((p) => t.startsWith(p.label))));

const legend = await page.$$eval('#bootLegend .lg', (els) => els.map((e) => e.textContent));
check('图例含合计行', legend.some((t) => t.includes('合计')), legend[legend.length - 1]?.trim());
check('图例覆盖每个阶段', cur.phases.every((p) => legend.some((t) => t.includes(p.label))));
check('可优化段有标记', cur.phases.filter((p) => p.actionable).every((p) =>
  legend.some((t) => t.includes(p.label) && t.includes('可优化'))));

const trendBars = await page.$$eval('#trendChart .bar', (els) => els.length);
check('趋势柱数 = 记录数', trendBars === boot.records.length, `${trendBars} vs ${boot.records.length}`);
check('最快/最慢出现在说明里', (await page.textContent('#bootSub')).includes('最快'));

console.log('\n== 3. 拖慢归因 ==');
const culprits = await page.$$eval('#culpritList li', (els) => els.map((e) => e.textContent));
if (cur.culprits.length) {
  check('归因条数一致', culprits.length === cur.culprits.length, `${culprits.length} vs ${cur.culprits.length}`);
  check('按拖慢降序', cur.culprits.every((c, i) => i === 0 || cur.culprits[i - 1].degradation_ms >= c.degradation_ms));
  check('第一名就是最大拖慢源', culprits[0].includes(cur.culprits[0].name), culprits[0]?.slice(0, 60));
  check('显示拖慢秒数', culprits[0].includes('拖慢'));
} else {
  check('无归因时隐藏卡片', await page.isHidden('#culpritCard'));
}

console.log('\n== 4. 启动项表格（两段式补签） ==');
const rowsAfterFast = await page.$$eval('#startupBody tr', (els) => els.length);
check('行数 = 条目数', rowsAfterFast === fast.items.length, `${rowsAfterFast} vs ${fast.items.length}`);

const wantCounts = Object.fromEntries(fast.counts);
const counts = await page.evaluate(() => ({
  run: document.querySelector('#cntRunKey').textContent,
  svc: document.querySelector('#cntService').textContent,
  task: document.querySelector('#cntTask').textContent,
  all: document.querySelector('#cntStartupAll').textContent,
}));
check('来源计数正确', counts.run === String(wantCounts.run_key)
  && counts.svc === String(wantCounts.service)
  && counts.task === String(wantCounts.scheduled_task)
  && counts.all === String(fast.items.length), JSON.stringify(counts));

await page.waitForFunction(
  (n) => !/正在确认/.test(document.querySelector('#startupSub').textContent)
     && document.querySelectorAll('#startupBody .risk-tag').length >= n,
  fast.items.length, { timeout: 15000 });

const subs2 = await page.evaluate(() => window.__TAURI_TEST__.subs);
const pendingIdx2 = subs2.findIndex((t) => /正在确认 \d+ 个程序的发布者/.test(t));
const settledIdx2 = subs2.findIndex((t) =>
  new RegExp(`^共 ${fast.items.length} 项 · 可勾选 \\d+`).test(t) && /· 扫描 \d+ ms/.test(t));
check('先出列表再补签（副标题先提示确认中）', pendingIdx2 >= 0 && settledIdx2 > pendingIdx2,
  subs2.map((s) => `「${s}」`).join(' → '));

const pubCount = await page.evaluate(() =>
  [...document.querySelectorAll('#startupBody .risk-reason')].filter((e) => e.textContent && !/确认中/.test(e.textContent)).length);
check('补签后发布者/理由已就位', pubCount > 0, `${pubCount} 条有结论`);

const dangerLocked = await page.evaluate(() => {
  const rows = [...document.querySelectorAll('#startupBody tr')];
  const d = rows.filter((r) => r.querySelector('.risk-tag.dangerous'));
  // 空数组会让 every()  vacuously 通过，所以先要求真的渲染出了危险档
  return { total: d.length, locked: d.length > 0 && d.every((r) => r.querySelector('input[type=checkbox]').disabled) };
});
check('危险档条目的勾选框被锁', dangerLocked.locked, `${dangerLocked.total} 条危险档`);

// 「可安全禁用」却说勾不上，是用户最容易误解的地方：那些条目其实**已经禁用**了。
// 影响列必须说「已禁用」并指向「启用」按钮，而不是继续念风险评级。
const offRows = await page.evaluate(() => {
  const rows = [...document.querySelectorAll('#startupBody tr')];
  const off = rows.filter((r) => r.querySelector('.startup-env.off'));
  return {
    total: off.length,
    taggedOff: off.filter((r) => r.querySelector('.risk-tag.off')).length,
    wrongSafe: off.filter((r) => r.querySelector('.risk-tag.safe')).length,
    hint: off.every((r) => /启用/.test(r.querySelector('.risk-reason')?.textContent || '')),
  };
});
check('已禁用行改标「已禁用」，不再显示「可安全禁用」',
  offRows.total > 0 && offRows.taggedOff === offRows.total && offRows.wrongSafe === 0,
  `${offRows.total} 行已禁用 / ${offRows.taggedOff} 行标注正确 / ${offRows.wrongSafe} 行仍标成可禁用`);
check('已禁用行说明了恢复入口', offRows.hint, offRows.hint ? '提示「点右侧启用可恢复」' : '缺少恢复提示');

const subBreakdown = await page.evaluate(() => document.querySelector('#startupSub').textContent);
const wantPick = fast.items.filter((i) => i.enabled && (i.can_disable || i.in_blacklist)).length;
const wantOff = fast.items.filter((i) => !i.enabled).length;
check('副标题写明可勾选 / 已禁用 / 不建议禁用的拆分',
  subBreakdown.includes(`可勾选 ${wantPick}`) && subBreakdown.includes(`已禁用 ${wantOff}`),
  subBreakdown);


const impactShown = await page.evaluate(() => {
  const texts = [...document.querySelectorAll('#startupBody .risk-cell')].map((e) => e.textContent);
  return texts.filter((t) => /拖慢开机/.test(t));
});
check('拖慢开机的启动项带影响标注', impactShown.length === fast.items.filter((i) => i.impact_ms).length,
  `${impactShown.length} 条，如：${impactShown[0]?.match(/拖慢开机[^ ]*[ ]?[^ ]*/)?.[0] || '无'}`);
check('延迟启动按钮出现在工具栏', await page.isVisible('#btnStartupDelay'));
const delayedRows = await page.evaluate(() =>
  [...document.querySelectorAll('#startupBody .startup-env')].filter((e) => /已延迟启动/.test(e.textContent)).length);
check('已延迟的服务有状态标注', delayedRows === fast.items.filter((i) => i.delayed).length,
  `${delayedRows} 行`);

console.log('\n== 5. 筛选与搜索 ==');
await page.click('#startupFilters .chip[data-src="service"]');
await page.waitForTimeout(200);
let svcRows = await page.$$eval('#startupBody tr', (els) => els.length);
const wantSvc = fast.items.filter((i) => i.source === 'service').length;
check('筛「服务」只剩服务项', svcRows === wantSvc, `${svcRows} vs ${wantSvc}`);
await page.click('#startupFilters .chip[data-src="all"]');
await page.waitForTimeout(200);

// 搜索词从 fixture 里取，换台机器也不会因为「没有这个启动项」而假失败
const probe = fast.items.find((i) => i.name.length >= 4)?.name || 'a';
await page.fill('#startupSearch', probe);
await page.waitForTimeout(350);
let qRows = await page.$$eval('#startupBody tr', (els) => els.map((e) => e.textContent));
check('搜索能过滤', qRows.length > 0
  && qRows.every((t) => t.toLowerCase().includes(probe.toLowerCase())), `${qRows.length} 行 / 关键词「${probe}」`);
await page.fill('#startupSearch', '');
await page.waitForTimeout(350);

console.log('\n== 6. 禁用流程（演练 → 确认 → 执行） ==');
// 只勾一个「可安全禁用」的条目，避免碰到任何真实写路径
const target = await page.evaluate(() => {
  const cb = [...document.querySelectorAll('#startupBody input[type=checkbox]')]
    .find((c) => !c.disabled);
  if (cb) { cb.click(); return cb.dataset.id; }
  return null;
});
check('能勾选可禁用项', !!target);
await page.waitForTimeout(120);
check('按钮显示已选数量', /禁用选中项（1）/.test(await page.textContent('#btnStartupDisable')));

await page.click('#btnStartupDisable');
await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
check('确认弹窗出现', await page.isVisible('#confirmMask'));
const modalItems = await page.$$eval('#confirmList li', (els) => els.map((e) => e.textContent));
check('弹窗列出真实演练清单', modalItems.length === 1, modalItems[0]?.slice(0, 50));

await page.click('#confirmOk');
await page.waitForFunction(() => document.querySelector('#confirmMask').hidden, { timeout: 8000 });
check('执行后弹窗关闭', true);
await page.waitForTimeout(600);
const toastText = await page.evaluate(() => document.querySelector('#toast').textContent);
check('给出结果提示', /已禁用|可随时还原/.test(toastText), toastText);

const batchVisible = await page.isVisible('#restoreCard');
check('变更记录卡片出现', batchVisible);
const batchRows = await page.$$eval('#batchList li', (els) => els.length);
check('批次可还原', batchRows === 1, String(batchRows));

console.log('\n== 6b. 延迟启动流程（仅服务） ==');
/* 从**当前页面上真正可勾选的行**里挑，而不是从 fixture 里挑。
   前面的禁用会让条目变成「已禁用」——按需求它仍留在列表里，只是不再可勾选；
   这时候 fixture 那份静态列表已经和页面不同步，照着它点会点到一个灰掉的框。 */
const svcPick = await page.evaluate(() => {
  for (const r of document.querySelectorAll('#startupBody tr')) {
    if ((r.querySelector('.startup-src')?.textContent || '').trim() !== '自动服务') continue;
    const cb = r.querySelector('input[type=checkbox]');
    if (!cb || cb.disabled) continue;
    if (!r.querySelector('button[data-act="delay"]')) continue;
    return cb.dataset.id;
  }
  return null;
});
const otherPick = await page.evaluate(() => {
  for (const r of document.querySelectorAll('#startupBody tr')) {
    const src = (r.querySelector('.startup-src')?.textContent || '').trim();
    const cb = r.querySelector('input[type=checkbox]');
    if (cb && !cb.disabled && src && src !== '自动服务') return cb.dataset.id;
  }
  return null;
});

if (svcPick) {
  await page.click(`#startupBody input[data-id="${svcPick}"]`);
  await page.waitForTimeout(150);
  const delayTxt = await page.textContent('#btnStartupDelay');
  check('勾选服务后延迟按钮可用', /延迟选中项（1）/.test(delayTxt), delayTxt);

  if (otherPick) {
    await page.click(`#startupBody input[data-id="${otherPick}"]`);
    await page.waitForTimeout(150);
    const t2 = await page.textContent('#btnStartupDelay');
    check('延迟按钮只统计可延迟的服务', /延迟选中项（1）/.test(t2), t2);
    const t3 = await page.textContent('#btnStartupDisable');
    check('禁用按钮统计全部勾选项', /禁用选中项（2）/.test(t3), t3);
    await page.click(`#startupBody input[data-id="${otherPick}"]`);
    await page.waitForTimeout(120);
  }

  const rowBtn = await page.$(`#startupBody button[data-act="delay"][data-id="${svcPick}"]`);
  check('服务行带行内「延迟」按钮', !!rowBtn);

  await page.click(`#startupBody button[data-act="delay"][data-id="${svcPick}"]`);
  await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
  const dTitle = await page.textContent('#confirmTitle');
  check('延迟确认弹窗文案正确', /延迟启动/.test(dTitle), dTitle);
  const dLead = await page.textContent('#confirmLead');
  check('弹窗说明延迟启动的语义', /开机关键路径走完后才拉起/.test(dLead), dLead.slice(0, 40));

  await page.click('#confirmOk');
  await page.waitForFunction(() => document.querySelector('#confirmMask').hidden, { timeout: 8000 });
  await page.waitForTimeout(900);
  const toast2 = await page.evaluate(() => document.querySelector('#toast').textContent);
  check('延迟结果提示', /已把 1 项设为延迟启动，下次开机生效/.test(toast2), toast2);
  const applied = await page.evaluate(() => window.__TAURI_TEST__.applied);
  check('后端收到 action=delay', applied.some((a) => a.action === 'delay' && a.id === svcPick));
  const okBtnText = await page.textContent('#confirmOk');
  check('确认按钮文案复位', okBtnText === '确认禁用', okBtnText);
} else {
  check('页面上有可延迟的服务（跳过说明这台机器没有）', true, '没有可延迟的服务，延迟流程跳过');
}

/* ============================================================
   9~15. P2 新增能力
   ------------------------------------------------------------
   用户报的核心问题：「全选点了没反应/勾不上」。根因不是逻辑，是**热区**：
   全选框当时只是工具栏里一个 15×15 的裸 checkbox，且位置在首屏视口之外。
   所以这里既要断言选择语义，也要把「可点面积」钉住 —— 只测语义的话，
   热区退回 15px 也照样能过。
   ============================================================ */
console.log('\n== 9. 选择交互（全选 / 一键选中推荐 / 清空） ==');

/** 清空当前勾选。按钮在「本来就没勾」时是禁用态，所以先探一下再点。 */
const clearPicked = async () => {
  if (!(await page.isDisabled('#btnClearPicked'))) {
    await page.click('#btnClearPicked');
    await page.waitForTimeout(180);
  }
};

const hit = await page.evaluate(() => {
  const m = (el) => {
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { w: Math.round(r.width), h: Math.round(r.height) };
  };
  return { all: m(document.querySelector('.startup-filter label.chk')), row: m(document.querySelector('#startupBody label.chk')) };
});
check('全选热区够大（不再是一个小方块）', hit.all && hit.all.w >= 56 && hit.all.h >= 28,
  hit.all ? `${hit.all.w}×${hit.all.h}` : '未找到');
check('行内勾选热区够高', hit.row && hit.row.h >= 24, hit.row ? `${hit.row.w}×${hit.row.h}` : '未找到');

// 还没勾任何东西时，「清空选择」必须是禁用态（否则点了没反应也是 bug）
check('未勾选时「清空选择」为禁用态',
  await page.isDisabled('#btnClearPicked'));

// 在「服务」筛选下点**全选 label**（模拟真实用户点文字/方块，而不是脚本点 input）
await page.click('#startupFilters .chip[data-src="service"]');
await page.waitForTimeout(250);
await page.click('.startup-filter label.chk');
await page.waitForTimeout(250);

const svcSel = await page.evaluate(() => {
  const cbs = [...document.querySelectorAll('#startupBody input[type=checkbox]')];
  const pick = cbs.filter((c) => !c.disabled);
  const all = document.querySelector('#chkStartupAll');
  return {
    rows: cbs.length,
    pickable: pick.length,
    checked: pick.filter((c) => c.checked).length,
    lockedChecked: cbs.filter((c) => c.disabled && c.checked).length,
    allChecked: all.checked,
    btn: document.querySelector('#btnStartupDisable').textContent,
  };
});
check('全选勾上了当前筛选内全部可勾选项',
  svcSel.pickable > 0 && svcSel.checked === svcSel.pickable,
  `勾 ${svcSel.checked}/${svcSel.pickable}（共 ${svcSel.rows} 行）`);
check('不建议禁用的条目没被全选勾上', svcSel.lockedChecked === 0, `locked=${svcSel.lockedChecked}`);
check('全选框自身显示已全选', svcSel.allChecked);
check('按钮计数与筛选内勾选数一致', svcSel.btn.includes(`（${svcSel.checked}）`), svcSel.btn);

// 全选只勾得动「可勾选」的行，剩下的必须当场解释 —— 否则用户看到
// 148 项只勾上 30 项，第一反应就是「全选坏了」。
const allToast = await page.evaluate(() => {
  const el = document.querySelector('#toast');
  return el && !el.hidden ? el.textContent : '';
});
check('全选后说明有多少项被跳过及原因', /已选中 \d+ 项；另有 \d+ 项不可勾选（已禁用 \d+、不建议禁用 \d+）/.test(allToast),
  allToast || '（没有提示）');

// 切回「全部」：勾选保留，但全选框必须如实反映「还有没勾的」
await page.click('#startupFilters .chip[data-src="all"]');
await page.waitForTimeout(250);
const allSel = await page.evaluate(() => {
  const pick = [...document.querySelectorAll('#startupBody input[type=checkbox]')].filter((c) => !c.disabled);
  const all = document.querySelector('#chkStartupAll');
  return {
    pickable: pick.length,
    checked: pick.filter((c) => c.checked).length,
    allChecked: all.checked,
    allPicked: pick.length > 0 && pick.every((c) => c.checked),
  };
});
check('切换筛选后全选框与实际勾选一致（不再「显示已全选却有空勾」）',
  allSel.allChecked === allSel.allPicked,
  `可见可勾 ${allSel.pickable}，已勾 ${allSel.checked}，全选框 ${allSel.allChecked}`);

await page.click('#btnClearPicked');
await page.waitForTimeout(200);
const afterClear = await page.evaluate(() => ({
  checked: [...document.querySelectorAll('#startupBody input[type=checkbox]')].filter((c) => c.checked).length,
  btn: document.querySelector('#btnStartupDisable').textContent,
  allChecked: document.querySelector('#chkStartupAll').checked,
  disabled: document.querySelector('#btnClearPicked').disabled,
}));
check('清空选择后没有任何勾选', afterClear.checked === 0, `checked=${afterClear.checked}`);
check('清空选择后按钮回到默认文案', afterClear.btn === '禁用选中项', afterClear.btn);
check('清空选择后全选框同步取消', !afterClear.allChecked);
check('清空选择后「清空」按钮回到禁用态', afterClear.disabled);

console.log('\n== 10. 一键选中推荐项 ==');
await clearPicked();
await page.click('#btnPickRecommended');
await page.waitForTimeout(300);
const recSel = await page.evaluate(() => {
  const rows = [...document.querySelectorAll('#startupBody tr')];
  const recRows = rows.filter((r) => {
    const cb = r.querySelector('input[type=checkbox]');
    // 推荐 = 可勾选 + 风险档「可安全禁用」+ 未加白（与 boot.js `recommended()` 同义）
    return cb && !cb.disabled && r.querySelector('.risk-tag.safe') && !r.querySelector('.badge.white');
  });
  return {
    rec: recRows.length,
    allPicked: recRows.length > 0 && recRows.every((r) => r.querySelector('input[type=checkbox]').checked),
    btn: document.querySelector('#btnPickRecommended').textContent,
    disableBtn: document.querySelector('#btnStartupDisable').textContent,
  };
});
check('推荐禁用的条目被一键全选', recSel.rec > 0 && recSel.allPicked, `推荐 ${recSel.rec} 条`);
check('选中后按钮提示已全部选中', /已全部选中|无推荐项/.test(recSel.btn), recSel.btn);
check('禁用按钮计数 = 推荐项数', recSel.disableBtn.includes(`（${recSel.rec}）`), recSel.disableBtn);
await clearPicked();

console.log('\n== 11. 启动项说明面板（知识库） ==');
const kbId = await page.evaluate(() => {
  for (const b of document.querySelectorAll('#startupBody button[data-info]')) {
    const line = b.closest('tr').querySelector('.kb-line');
    if (line && !line.classList.contains('none')) return b.dataset.info;
  }
  return null;
});
check('列表里存在带内置说明的条目', !!kbId);

await page.click(`#startupBody button[data-info="${kbId}"]`);
await page.waitForSelector('#infoMask:not([hidden])', { timeout: 5000 });
const info = await page.evaluate(() => ({
  name: document.querySelector('#infoName').textContent,
  origin: document.querySelector('#infoOrigin').textContent,
  cmd: document.querySelector('#infoCmd').textContent,
  desc: (document.querySelector('#infoAdvice .adv-desc')?.textContent || '').trim(),
  advTitle: (document.querySelector('#infoAdvice .adv-title')?.textContent || '').trim(),
  tags: document.querySelector('#infoTags').children.length,
}));
check('说明面板打开并显示条目名', info.name.length > 0, info.name.slice(0, 26));
check('标注说明来自内置库', info.origin === '内置说明', info.origin);
check('给出综合描述（知识库的核心价值）', info.desc.length > 20, `${info.desc.length} 字：${info.desc.slice(0, 28)}…`);
check('给出标题与厂商', info.advTitle.length > 0, info.advTitle.slice(0, 30));
check('显示命令行原文', info.cmd.length > 0, info.cmd.slice(0, 40));
check('标签行有来源与状态', info.tags >= 2, String(info.tags));

// 手动编辑（用户报的「未知进程用户可以手动编辑其信息」）
await page.click('#infoEditBtn');
await page.waitForSelector('#infoEdit:not([hidden])', { timeout: 5000 });
check('编辑区展开', await page.isVisible('#infoEdit'));
await page.fill('#edKey', 'e2e_probe_key');
await page.fill('#edTitle', '端到端测试条目');
await page.fill('#edVendor', '测试厂商');
await page.fill('#edDesc', '这条说明由无头测试写入，用来验证「未知进程可以手动编辑其信息」这条需求。');
await page.selectOption('#edAdvice', '建议保留');
await page.click('#infoSave');
await page.waitForTimeout(500);
const kbHits = await page.evaluate(() => window.__TAURI_TEST__.kbHits);
check('保存说明把内容送进后端',
  kbHits.some((h) => h.op === 'save' && h.key === 'e2e_probe_key' && h.title === '端到端测试条目' && h.advice === '建议保留'),
  JSON.stringify(kbHits[kbHits.length - 1] || null));

// 保存会触发一次重扫（列表先回「未补签」态），必须等风险档补签回来，
// 否则下一组按 `.risk-tag.safe` 找样本会扑空 —— 这是流程约束，不是产品 bug。
await page.click('#infoClose');
await page.waitForTimeout(200);
await page.waitForFunction(
  () => document.querySelectorAll('#startupBody .risk-tag.safe').length > 0,
  { timeout: 10000 }).catch(() => {});

/* 用户的原话是「未知进程用户可以手动编辑其信息」—— 上面测的是**已有内置说明**的
   条目，这一组必须单独测「库里查不到」的条目：面板要如实说「暂无说明」，
   编辑区要能打开、能存、存完这条就变成「你的补充」。 */
console.log('\n== 11b. 未收录条目也能手动补充说明 ==');
const unknownId = await page.evaluate(() => {
  for (const r of document.querySelectorAll('#startupBody tr')) {
    if (r.querySelector('.kb-line.none')) return r.querySelector('button[data-info]')?.dataset.info || null;
  }
  return null;
});
check('存在没有内置说明的条目（正好用来验证「手动编辑」）', !!unknownId);

if (unknownId) {
  await page.click(`#startupBody button[data-info="${unknownId}"]`);
  await page.waitForSelector('#infoMask:not([hidden])', { timeout: 5000 });
  const unk = await page.evaluate(() => ({
    origin: document.querySelector('#infoOrigin').textContent,
    advice: document.querySelector('#infoAdvice').textContent,
    editBtnHidden: document.querySelector('#infoEditBtn').hidden,
  }));
  check('未收录条目如实标注「暂无说明」', unk.origin === '暂无说明', unk.origin);
  check('未收录条目提示用户可以自己补', /没有这个启动项的说明/.test(unk.advice),
    unk.advice.slice(0, 34));
  check('未收录条目同样能打开编辑区', unk.editBtnHidden === false);

  await page.click('#infoEditBtn');
  await page.waitForSelector('#infoEdit:not([hidden])', { timeout: 5000 });
  const prefilled = await page.inputValue('#edKey');
  check('编辑区预填了可执行文件名（省得用户自己想匹配串）', prefilled.length > 0, prefilled.slice(0, 30));
  await page.fill('#edTitle', '用户自己补的条目');
  await page.fill('#edDesc', '这个程序内置库里没有，由用户手动补写的说明。');
  await page.selectOption('#edAdvice', '可以禁用');
  await page.click('#infoSave');
  await page.waitForTimeout(700);
  const saved = await page.evaluate(() => window.__TAURI_TEST__.kbHits.slice(-1)[0]);
  check('用户补写的说明能存进外置 JSON',
    saved && saved.op === 'save' && saved.title === '用户自己补的条目',
    JSON.stringify(saved || null));
  await page.click('#infoClose');
  await page.waitForTimeout(200);
  await page.waitForFunction(
    () => document.querySelectorAll('#startupBody .risk-tag.safe').length > 0,
    { timeout: 10000 }).catch(() => {});
}

console.log('\n== 12. 白名单 / 黑名单 ==');
/** 找一条「可勾选 + 可安全禁用 + 未加白」的条目 id。
 *  注意：行内「说明」按钮上挂的是 `data-info`（值即条目 id），没有 `data-id`。 */
const findSafe = () => page.evaluate(() => {
  for (const r of document.querySelectorAll('#startupBody tr')) {
    const cb = r.querySelector('input[type=checkbox]');
    if (cb && !cb.disabled && r.querySelector('.risk-tag.safe') && !r.querySelector('.badge.white')) {
      return r.querySelector('button[data-info]')?.dataset.info || null;
    }
  }
  return null;
});

const wlId = await findSafe();
check('找到一条可安全禁用的条目', !!wlId);

await page.click(`#startupBody button[data-info="${wlId}"]`);
await page.waitForSelector('#infoMask:not([hidden])');
await page.click('#infoWhite');
await page.waitForTimeout(600);
const wlState = await page.evaluate(() => ({
  badge: document.querySelectorAll('#startupBody .badge.white').length,
  btn: document.querySelector('#infoWhite').textContent,
}));
check('加白后列表出现「已加白」标记', wlState.badge >= 1, `badge=${wlState.badge}`);
check('按钮切到「移出白名单」', /移出白名单/.test(wlState.btn), wlState.btn);

await page.click('#infoClose');
await page.waitForTimeout(200);
await clearPicked();
await page.click('#btnPickRecommended');
await page.waitForTimeout(300);
const wlExcluded = await page.evaluate((id) => {
  const cb = document.querySelector(`#startupBody input[data-id="${id}"]`);
  return cb ? !cb.checked : null;
}, wlId);
check('加白的条目不再被「一键选中推荐项」勾上', wlExcluded === true, String(wlExcluded));

// 黑名单：免确认，立即禁用
const blId = await findSafe();
check('找到一条可用的黑名单样本', !!blId);
if (blId) {
  await page.click(`#startupBody button[data-info="${blId}"]`);
  await page.waitForSelector('#infoMask:not([hidden])');
  await page.click('#infoBlack');
  await page.waitForTimeout(700);
  const blToast = await page.evaluate(() => document.querySelector('#toast').textContent);
  check('加入黑名单免二次确认、直接禁用', /直接禁用|已加入黑名单/.test(blToast), blToast);
  const blState = await page.evaluate(() => ({
    black: document.querySelectorAll('#startupBody .badge.black').length,
    mine: document.querySelectorAll('#startupBody .badge.mine').length,
    rows: document.querySelectorAll('#startupBody tr').length,
  }));
  check('黑名单标记出现', blState.black >= 1, `black=${blState.black}`);
  check('被禁用的条目仍留在列表里（不再从列表消失）', blState.rows >= fast.items.length,
    `${blState.rows} 行 vs 扫描到 ${fast.items.length} 条`);
  check('留在列表里的禁用项带「老六禁用」标记', blState.mine >= 1, `mine=${blState.mine}`);
  // 收尾：把说明面板关掉，否则它的遮罩会挡住后面要点的行内按钮
  await page.click('#infoClose');
  await page.waitForTimeout(250);
}

console.log('\n== 13. 已禁用项一键启用 ==');
const enTarget = await page.evaluate(() => {
  const b = document.querySelector('#startupBody button[data-act="enable"]');
  return b ? { id: b.dataset.id, name: b.closest('tr').querySelector('.startup-name').textContent } : null;
});
check('已禁用条目带「启用」按钮', !!enTarget, enTarget ? enTarget.name.slice(0, 26) : '未找到');
if (enTarget) {
  const before = await page.evaluate(() => window.__TAURI_TEST__.applied.length);
  await page.click(`#startupBody button[data-act="enable"][data-id="${enTarget.id}"]`);
  await page.waitForTimeout(700);
  const after = await page.evaluate(() => window.__TAURI_TEST__.applied);
  check('一键启用直接执行（不弹确认框）',
    after.length > before && after[after.length - 1].action === 'enable',
    JSON.stringify(after[after.length - 1] || null));
  const back = await page.evaluate((id) => {
    const cb = document.querySelector(`#startupBody input[data-id="${id}"]`);
    return cb ? !cb.disabled : null;
  }, enTarget.id);
  check('启用后该条恢复为可禁用状态', back === true, String(back));
}

console.log('\n== 13b. 受保护的服务：确认前就说清「改不动」 ==');
// 真机复现：某些服务（安全软件自保护 / 只允许 SYSTEM）即使管理员也拿不到
// SERVICE_CHANGE_CONFIG。以前演练不探权限，确认清单照旧承诺「可还原」，
// 点下去才弹「拒绝访问 0x80070005」—— 用户只会觉得按钮坏了。
await clearPicked();
await page.click('#startupFilters .chip[data-src="service"]');
await page.waitForTimeout(250);
const twoSvc = await page.evaluate(() => {
  const rows = [...document.querySelectorAll('#startupBody tr')];
  const pick = rows
    .map((r) => r.querySelector('input[type=checkbox]'))
    .filter((cb) => cb && !cb.disabled)
    .slice(0, 2)
    .map((cb) => cb.dataset.id);
  return pick;
});
if (twoSvc.length === 2) {
  await page.evaluate((id) => window.__TAURI_TEST__.mockState.protectedIds.add(id), twoSvc[1]);
  for (const id of twoSvc) await page.click(`#startupBody input[data-id="${id}"]`);
  await page.click('#btnStartupDisable');
  await page.waitForTimeout(400);

  const dlg = await page.evaluate(() => {
    const items = [...document.querySelectorAll('#confirmList li')];
    const blocked = items.filter((li) => li.classList.contains('blocked'));
    return {
      open: !document.querySelector('#confirmMask').hidden,
      total: items.length,
      blocked: blocked.length,
      why: blocked[0]?.textContent || '',
      okEnabled: !document.querySelector('#confirmOk').disabled,
    };
  });
  check('确认清单把「改不动」的条目单独标出来', dlg.open && dlg.blocked === 1 && dlg.total === 2,
    `共 ${dlg.total} 条，其中 ${dlg.blocked} 条标为不会改动`);
  check('并写明真实原因（受保护 / 拒绝修改）', /受保护|拒绝修改/.test(dlg.why), dlg.why.slice(0, 52));
  check('可执行的那条仍能继续确认', dlg.okEnabled);

  await page.click('#confirmCancel');
  await page.waitForTimeout(200);
  await page.evaluate((id) => window.__TAURI_TEST__.mockState.protectedIds.delete(id), twoSvc[1]);
  await clearPicked();
  await page.click('#startupFilters .chip[data-src="all"]');
  await page.waitForTimeout(200);
} else {
  check('找到两条可勾选的服务用于受保护场景', false, `只有 ${twoSvc.length} 条`);
}

console.log('\n== 14. 删除只对可删来源开放 ==');
const idOfSource = (label) => page.evaluate((t) => {
  for (const r of document.querySelectorAll('#startupBody tr')) {
    if ((r.querySelector('.startup-src')?.textContent || '').trim() === t) {
      return r.querySelector('button[data-info]')?.dataset.info || null;
    }
  }
  return null;
}, label);

const runId = await idOfSource('注册表');
if (runId) {
  await page.click(`#startupBody button[data-info="${runId}"]`);
  await page.waitForSelector('#infoMask:not([hidden])');
  const del = await page.evaluate(() => ({
    disabled: document.querySelector('#infoDelete').disabled,
    hint: document.querySelector('#infoHint').textContent,
  }));
  check('注册表项允许删除', del.disabled === false);
  check('删除前会说明「已备份原值」', /备份/.test(del.hint), del.hint.slice(0, 40));
  await page.click('#infoClose');
  await page.waitForTimeout(200);
} else {
  check('fixture 里有注册表来源的条目', false, '未找到');
}

const svcId = await idOfSource('自动服务');
if (svcId) {
  await page.click(`#startupBody button[data-info="${svcId}"]`);
  await page.waitForSelector('#infoMask:not([hidden])');
  const del = await page.evaluate(() => ({
    disabled: document.querySelector('#infoDelete').disabled,
    hint: document.querySelector('#infoHint').textContent,
  }));
  check('服务类不允许删除自启（并说明原因）', del.disabled === true);
  check('对不可删来源给出替代方案', /禁用/.test(del.hint), del.hint.slice(0, 40));
  await page.click('#infoClose');
  await page.waitForTimeout(200);
} else {
  check('fixture 里有服务来源的条目', false, '未找到');
}

console.log('\n== 15. 设置抽屉里的知识库 / 名单 ==');
await page.click('.nav-item[data-view="settings"]');
await page.waitForSelector('#drawer:not([hidden])', { timeout: 5000 });
await page.waitForTimeout(600);
const drawer = await page.evaluate(() => ({
  builtin: document.querySelector('#kbBuiltin').textContent.trim(),
  user: document.querySelector('#kbUser').textContent.trim(),
  path: document.querySelector('#kbPath').textContent.trim(),
  kbRows: document.querySelectorAll('#kbList li').length,
  listRows: document.querySelectorAll('#listRules li').length,
  listPath: document.querySelector('#listPath').textContent.trim(),
}));
check('展示内置库条目数', Number(drawer.builtin) > 100, drawer.builtin);
check('展示用户补充条目数与文件路径', Number(drawer.user) >= 1 && drawer.path.length > 0,
  `${drawer.user} 条 / ${drawer.path}`);
check('用户补充的说明在设置页可见', drawer.kbRows >= 1, `${drawer.kbRows} 行`);
check('白/黑名单内容在设置页可见', drawer.listRows >= 1, `${drawer.listRows} 行`);
check('名单文件路径可见', drawer.listPath.length > 0, drawer.listPath);
await page.click('#drawerClose');
await page.waitForTimeout(250);
check('关闭后抽屉收起', await page.isHidden('#drawer'));

console.log('\n== 16. 默认排序与「隐藏已禁用」 ==');
await page.click('#startupFilters .chip[data-src="all"]');
await page.waitForTimeout(200);
await clearPicked();

// 打开这一页就该先看到「该关哪些」，所以推荐项必须排在最前面。
const order = await page.evaluate(() => {
  const rows = [...document.querySelectorAll('#startupBody tr')];
  const rank = (r) => {
    const cb = r.querySelector('input[type=checkbox]');
    const safe = !!r.querySelector('.risk-tag.safe');
    const white = !!r.querySelector('.badge.white');
    const off = !!r.querySelector('.startup-env.off');
    if (cb && !cb.disabled && safe && !white) return 0; // 推荐
    if (cb && !cb.disabled) return 1;                   // 其他可勾选
    return off ? 3 : 2;                                 // 不建议禁用 / 已禁用
  };
  const ranks = rows.map(rank);
  return {
    ranks,
    firstRank: ranks[0],
    recommended: ranks.filter((x) => x === 0).length,
    sorted: ranks.every((v, i) => i === 0 || ranks[i - 1] <= v),
    lastIsOff: ranks[ranks.length - 1] === 3,
  };
});
check('推荐禁用的条目排在最前', order.firstRank === 0 && order.recommended > 0,
  `推荐 ${order.recommended} 项，首行档位 ${order.firstRank}`);
check('整张表按「推荐 → 可勾选 → 不建议 → 已禁用」排列', order.sorted,
  order.ranks.join(''));
check('已禁用的条目垫底', order.lastIsOff);

const beforeHide = await page.evaluate(() => ({
  rows: document.querySelectorAll('#startupBody tr').length,
  // 用**当前 DOM** 里的已禁用行数，而不是 fixture 里的：
  // 前面的用例已经真的禁用过几条，fixture 的数字对不上了。
  off: document.querySelectorAll('#startupBody .startup-env.off').length,
}));
await page.check('#chkHideDisabled');
await page.waitForTimeout(300);
const hidden = await page.evaluate(() => ({
  rows: document.querySelectorAll('#startupBody tr').length,
  offRows: document.querySelectorAll('#startupBody .startup-env.off').length,
  sub: document.querySelector('#startupSub').textContent,
}));
check('隐藏已禁用后表格里不再有已禁用行',
  hidden.offRows === 0 && hidden.rows === beforeHide.rows - beforeHide.off,
  `${hidden.rows} 行（原 ${beforeHide.rows}，应减 ${beforeHide.off}）`);
check('副标题说明隐藏了多少项',
  new RegExp(`已隐藏 ${beforeHide.off} 项已禁用`).test(hidden.sub), hidden.sub);

await page.uncheck('#chkHideDisabled');
await page.waitForTimeout(300);
const restored = await page.$$eval('#startupBody tr', (els) => els.length);
check('取消隐藏后已禁用行回到列表', restored === beforeHide.rows, `${restored} vs ${beforeHide.rows}`);

// 开机加速页截图必须在切回进程页**之前**拍，否则拍到的是净化页；
// 还要先把内层滚动容器滚回顶部 —— 前面的点击会把视图滚到表格中段，
// 那样拍出来只有启动项表格，没有时间线和归因卡。
await page.evaluate(() => {
  const v = document.querySelector('#viewBoot');
  if (v) v.scrollTop = 0;
  window.scrollTo(0, 0);
});
await page.screenshot({ path: join(OUT, 'boot-view.png'), fullPage: true });
console.log(`\n截图：${join(OUT, 'boot-view.png')}`);

console.log('\n== 7. 切回进程页 ==');
await page.click('.nav-item[data-view="purge"]');
await page.waitForSelector('#viewPurge:not([hidden])');
check('进程页恢复显示', await page.isVisible('#viewPurge'));
check('刷新按钮回到进程页', await page.isVisible('#btnRefresh'));
check('开机加速页收起', await page.isHidden('#viewBoot'));

console.log('\n== 8. 无 JS 报错 ==');
check('零 pageerror / console.error', pageErrors.length === 0, pageErrors.slice(0, 3).join(' | '));

// 补一张弹窗截图
await page.click('.nav-item[data-view="boot"]');
const target2 = await page.evaluate(() => {
  const cb = [...document.querySelectorAll('#startupBody input[type=checkbox]')].find((c) => !c.disabled);
  if (cb) { cb.click(); return true; }
  return false;
});
if (target2) {
  await page.click('#btnStartupDisable');
  await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
  await page.screenshot({ path: join(OUT, 'boot-confirm.png') });
  console.log(`截图：${join(OUT, 'boot-confirm.png')}`);
  await page.click('#confirmCancel');
}

await browser.close();

const failed = results.filter((r) => !r.pass);
console.log(`\n================ 结果：${results.length - failed.length}/${results.length} 通过 ================`);
if (failed.length) {
  console.log('未通过：');
  for (const f of failed) console.log('  - ' + f.name + (f.detail ? ' (' + f.detail + ')' : ''));
  process.exit(1);
}
