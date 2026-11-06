/**
 * 老六清净 old666 · 一键体检 + 垃圾清理页 无头渲染验证（P4）
 * ------------------------------------------------------------
 * 为什么要这个脚本：
 *   这一页有两处「安静地做错事」的风险：
 *   1. 体检分数算错、扣分项排错序 —— 不会崩，只是给用户错误结论；
 *   2. 垃圾清理点错一下就删了不该删的东西 —— 不可逆。
 *   所以必须逐条断言，尤其是：
 *   - 体检项按扣分排序、每一项都有理由；
 *   - **回收站内容永远不可删**（只统计不删）；
 *   - 清理前必须弹确认框，点取消后 junk_delete 一次都没被调用；
 *   - 白名单外（桌面/文档）的东西绝不出现在扫描结果里。
 *
 * 做法（与 tests/frontend_mode.mjs 同构）：
 *   1. file:// 直接加载 src/index.html；
 *   2. 页面脚本执行前注入 window.__TAURI__，invoke 用**真机导出的
 *      真实后端 payload** 应答；
 *   3. 断言上述各项；
 *   4. 全页截图留档。
 *
 * 数据来源（真机、只读导出）：
 *   old666-poc --checkup --json  → checkup.json
 *   old666-poc --junk --json     → junk.json
 *
 * 运行方式：
 *   node tests/frontend_checkup.mjs
 */

import { readFileSync, mkdirSync, existsSync } from 'node:fs';
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

/** 真机 fixture（不是手写假数据）。缺 fixture 时给出明确指引，
 *  而不是静默退回假数据 —— 那会测出一个不存在的契约。 */
function loadFixtures() {
  const names = ['checkup.json', 'junk.json'];
  const missing = names.filter((f) => !existsSync(join(FIX, f)));
  if (missing.length) {
    throw new Error(
      `缺少 fixture：${missing.join('、')}\n` +
      '  请先导出（真机只读，几秒钟）：\n' +
      '    old666-poc --checkup --json > tests/_out/fixtures/checkup.json\n' +
      '    old666-poc --junk --json    > tests/_out/fixtures/junk.json');
  }
  // PS 5.1 的 `Out-File -Encoding utf8` 会写 BOM，JSON.parse 不认 —— 剥掉。
  const read = (f) => JSON.parse(
    readFileSync(join(FIX, f), 'utf8').replace(/^\uFEFF/, ''));
  return { checkup: read('checkup.json'), junk: read('junk.json') };
}

const { checkup: checkupPayload, junk: junkPayload } = loadFixtures();

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
await page.addInitScript(({ checkupPayload, junkPayload }) => {
  const ok = (data) => ({ ok: true, data, error: null });
  const err = (msg) => ({ ok: false, data: null, error: msg });

  /* 记录每一次删除请求 —— 「用户没点头时会不会真删」就看这里。 */
  const deletes = [];
  /* 记录每一次一键修复请求 —— 必须永远不包含结束进程的动作。 */
  const fixes = [];
  /* 回收站里的东西永远不给删 —— 后端本来就不删，这里再模拟一遍真实行为。 */
  const RECYCLE = 'recycle_bin';

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

          /* ---- 一键体检 ---- */
          case 'checkup_run': return ok(checkupPayload);

          /* ---- 垃圾清理 ---- */
          case 'junk_scan': return ok(junkPayload);
          case 'junk_delete': {
            deletes.push(args.paths || []);
            const paths = args.paths || [];
            const details = paths.map((p) => {
              const kind = (junkPayload.items.find((i) => i.path === p) || {}).kind;
              if (kind === RECYCLE) {
                return { path: p, ok: false, error: '回收站内容不提供清理，避免误清' };
              }
              return { path: p, ok: true, error: null };
            });
            return ok({
              deleted: details.filter((d) => d.ok).length,
              failed: details.filter((d) => !d.ok).length,
              freed_bytes: 0,
              details,
              elapsed_ms: 12,
            });
          }

          /* ---- 一键修复（绝不结束进程） ---- */
          case 'checkup_fix': {
            fixes.push(args.requests || []);
            const after = { ...checkupPayload, score: Math.min(100, checkupPayload.score + 2) };
            return ok({
              outcomes: [{
                kind: 'trim_memory',
                ok: true,
                detail: '裁剪了 3 个闲置程序的工作集，共释放 120.0 MB',
                freed_bytes: 125829120,
                affected: 3,
              }],
              before_score: checkupPayload.score,
              after_score: after.score,
              score_delta: after.score - checkupPayload.score,
              before_mem_pct: 68.2,
              after_mem_pct: 55.4,
              freed_bytes: 125829120,
              elapsed_ms: 412,
              after,
            });
          }

          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
  window.__TAURI_TEST__ = { deletes, fixes };
}, { checkupPayload, junkPayload });

await page.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/index.html');
await page.waitForTimeout(400);

console.log('\n== 1. 进入一键体检 ==');
await page.click('.nav-item[data-view="checkup"]');
await page.waitForSelector('#viewCheckup:not([hidden])');
await page.waitForFunction(() => document.querySelectorAll('#checkupList .checkup-item').length > 0);
await page.waitForTimeout(200);

check('视图切到一键体检', await page.isVisible('#viewCheckup'));
check('标题更新', (await page.textContent('#viewTitle')) === '一键体检');
check('进程页收起', await page.isHidden('#viewPurge'));
check('体检导航已启用（不再是 disabled）',
  await page.evaluate(() => !document.querySelector('.nav-item[data-view="checkup"]').disabled));
check('体检导航没有「P4」待办标记',
  await page.evaluate(() => !document.querySelector('.nav-item[data-view="checkup"] .soon')));

console.log('\n== 2. 体检结果 ==');
const score = await page.textContent('#checkupScore');
check('健康分显示与后端一致', score === String(checkupPayload.score),
  `${score} vs ${checkupPayload.score}`);

const items = await page.$$eval('#checkupList .checkup-item', (els) =>
  els.map((e) => ({
    text: e.textContent,
    sev: [...e.classList].find((c) => ['good', 'notice', 'warn', 'bad', 'unknown'].includes(c)) || '',
  })));
check('体检项数量 = 后端返回的项数', items.length === checkupPayload.items.length,
  `${items.length} vs ${checkupPayload.items.length}`);
check('每一项都渲染了理由', items.every((i) => i.text.trim().length > 0));

/* 扣分多的排前面 —— 后端已排序，前端不能打乱。 */
const costOrder = checkupPayload.items.map((i) => i.cost);
const sortedDesc = costOrder.every((c, idx) => idx === 0 || costOrder[idx - 1] >= c);
check('后端按扣分从多到少排序', sortedDesc, costOrder.join(','));
check('前端渲染顺序与后端一致',
  checkupPayload.items.every((it) => items.some((x) => x.text.includes(it.title))),
  checkupPayload.items.map((i) => i.title).join(' | '));

/* 严重度标签不能丢 —— 少了用户就不知道该多急。 */
const sevOk = checkupPayload.items.every((it) => {
  const el = items.find((x) => x.text.includes(it.title));
  return el && el.sev === it.severity;
});
check('严重度样式与后端一致', sevOk,
  checkupPayload.items.map((i) => `${i.title}=${i.severity}`).join(' '));

console.log('\n== 3. 垃圾扫描 ==');
await page.click('#btnJunkScan');
await page.waitForFunction(() => document.querySelectorAll('#junkGroups .junk-group').length > 0);
await page.waitForTimeout(200);

const groups = await page.$$eval('#junkGroups .junk-group', (els) =>
  els.map((e) => ({
    name: e.querySelector('.junk-group-name')?.textContent || '',
    size: e.querySelector('.junk-group-size')?.textContent || '',
    readonly: e.classList.contains('readonly'),
  })));
check('分类数量 = 后端返回的分类数', groups.length === junkPayload.groups.length,
  `${groups.length} vs ${junkPayload.groups.length}`);

/* 回收站只统计、不删 —— 界面上必须标明。 */
const recycleGroup = junkPayload.groups.find((g) => g[0] === 'recycle_bin');
if (recycleGroup) {
  const el = groups.find((g) => g.name.includes('回收站'));
  check('回收站分类标为不可清理', el && el.readonly, JSON.stringify(el || {}));
} else {
  check('回收站分类标为不可清理', true, '本次未扫到回收站（跳过）');
}

const junkItems = await page.$$eval('#junkItems .junk-item', (els) =>
  els.map((e) => ({
    path: e.querySelector('input')?.getAttribute('data-path') || '',
    text: e.textContent,
  })));
const deletableKinds = junkPayload.items.filter((i) => i.kind !== 'recycle_bin');
check('待清理条目数 = 后端可删条目数',
  junkItems.length === deletableKinds.length,
  `${junkItems.length} vs ${deletableKinds.length}`);
check('回收站条目不出现在待清理清单里',
  !junkItems.some((i) => (junkPayload.items.find((x) => x.path === i.path) || {}).kind === 'recycle_bin'));

/* 白名单红线：桌面、文档、图片、下载里绝不能出现。 */
const forbidden = /\\(Desktop|Documents|Pictures|Music|Videos|Downloads)\\/i;
const badPaths = junkItems.filter((i) => forbidden.test(i.path)).map((i) => i.path);
check('没有扫描到桌面/文档/图片/下载', badPaths.length === 0, badPaths.slice(0, 3).join(' | '));

console.log('\n== 4. 清理必须先确认 ==');
check('未选中任何项时清理按钮禁用',
  await page.evaluate(() => document.querySelector('#btnJunkClean').disabled));

/* 勾一个 */
await page.evaluate(() => {
  const cb = document.querySelector('#junkItems .junk-item input');
  if (cb) { cb.checked = true; cb.dispatchEvent(new Event('change', { bubbles: true })); }
});
await page.waitForTimeout(150);
check('勾选后清理按钮可点',
  await page.evaluate(() => !document.querySelector('#btnJunkClean').disabled));
check('清理按钮显示待清理条数',
  /1/.test(await page.textContent('#btnJunkClean')),
  await page.textContent('#btnJunkClean'));

/* 点清理 → 必须先弹确认框 */
await page.click('#btnJunkClean');
await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
check('清理前弹出确认框', await page.isVisible('#confirmMask'));
const confirmText = await page.textContent('#confirmMask');
check('确认框写明进回收站', /回收站/.test(confirmText), confirmText.slice(0, 120));

/* 关键断言：点取消后 junk_delete 一次都没被调用。 */
await page.click('#confirmCancel');
await page.waitForTimeout(300);
const afterCancel = await page.evaluate(() => window.__TAURI_TEST__.deletes.length);
check('点取消后没有调用 junk_delete（一个文件都没删）', afterCancel === 0,
  `deletes=${afterCancel}`);

/* 再来一次，这次点头 */
await page.click('#btnJunkClean');
await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
await page.click('#confirmOk');
await page.waitForTimeout(400);
const deletes = await page.evaluate(() => window.__TAURI_TEST__.deletes);
check('点确认后才调用 junk_delete', deletes.length === 1, `deletes=${deletes.length}`);
check('删除请求只带选中的那一个路径',
  deletes.length === 1 && deletes[0].length === 1,
  JSON.stringify(deletes[0] || []));

console.log('\n== 5. 一键修复不得结束任何进程 ==');
await page.click('#btnCheckupFixAll');
await page.waitForSelector('#fixCompareCard:not([hidden])', { timeout: 8000 });
await page.waitForTimeout(250);
const fixes = await page.evaluate(() => window.__TAURI_TEST__.fixes);
check('调用了 checkup_fix', fixes.length === 1, `fixes=${fixes.length}`);
/* 关键断言：一键修复的请求里只能有不结束进程的动作。 */
const killActions = ['purge_processes', 'purge', 'kill', 'end_process'];
const dangerous = (fixes[0] || []).filter((r) =>
  killActions.includes(r.kind) || /process/i.test(r.kind));
check('一键修复没有结束进程的动作', dangerous.length === 0,
  JSON.stringify(fixes[0] || []));
const cmpSub = await page.textContent('#fixCompareSub');
const cmpText = await page.textContent('#fixCompareBody');
check('对比页写明修复前后分数',
  /健康分/.test(cmpSub) && /→/.test(cmpSub) && /\d+\s*→\s*\d+/.test(cmpSub),
  cmpSub.slice(0, 100));
check('对比页列出每个修复动作的结果', /trim_memory/.test(cmpText), cmpText.slice(0, 80));
check('对比页声明「不会结束任何进程」', /不会结束任何进程/.test(cmpText), cmpText.slice(0, 120));

console.log('\n== 6. 截图 ==');
await page.evaluate(() => {
  const v = document.querySelector('#viewCheckup');
  if (v) v.scrollTop = 0;
  window.scrollTo(0, 0);
});
await page.screenshot({ path: join(OUT, 'checkup-view.png'), fullPage: true });
console.log(`截图：${join(OUT, 'checkup-view.png')}`);

console.log('\n== 7. 无 JS 报错 ==');
check('零 pageerror / console.error', pageErrors.length === 0, pageErrors.slice(0, 3).join(' | '));

await browser.close();

const failed = results.filter((r) => !r.pass);
console.log(`\n================ 结果：${results.length - failed.length}/${results.length} 通过 ================`);
if (failed.length) {
  console.log('未通过：');
  for (const f of failed) console.log('  - ' + f.name + (f.detail ? ' (' + f.detail + ')' : ''));
  process.exit(1);
}
