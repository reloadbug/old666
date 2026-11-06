/**
 * 老六清净 old666 · 实时监控页 + 悬浮球 无头渲染验证（P5）
 * ------------------------------------------------------------
 * 为什么要这个脚本：
 *   监控页是「常驻」的那一页 —— 它一直在后台跑，采样数字还直接决定
 *   「要不要报警」。如果阈值显示、单位换算、开关状态渲染错了，
 *   不会崩，只会安静地给错数字。
 *   悬浮球更要紧：它是**独立的一个 webview 窗口**，加载的是另一个
 *   HTML，它的 JS 跟主界面完全不相干 —— 主界面的测试根本碰不到它。
 *
 * 做法（与 tests/frontend_mode.mjs / frontend_checkup.mjs 同构）：
 *   1. file:// 直接加载 src/index.html；
 *   2. 页面脚本执行前注入 window.__TAURI__，invoke 用**真机导出的
 *      真实后端 payload**（old666-poc --monitor --json）应答；
 *   3. 断言：监控卡片渲染、告警列表、阈值回填、开关行为、
 *      悬浮球控件、**监控页没有任何结束进程的入口**；
 *   4. 悬浮球单独加载 src/floatball.html 跑一遍它自己的断言；
 *   5. 全页截图留档。
 *
 * 数据来源（真机、只读导出，缺了就由本脚本自动跑 CLI 生成）：
 *   old666-poc --monitor --json → tests/_out/fixtures/monitor.json
 *
 * 运行方式：
 *   node tests/frontend_monitor.mjs
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

/** 真机 fixture（不是手写假数据）。缺 fixture 时给出明确指引。 */
function loadFixtures() {
  const names = ['monitor.json'];
  const missing = names.filter((f) => !existsSync(join(FIX, f)));
  if (missing.length) {
    throw new Error(
      `缺少 fixture：${missing.join('、')}\n` +
      '  请先导出（真机只读，几秒钟）：\n' +
      '    old666-poc --monitor --json > tests/_out/fixtures/monitor.json');
  }
  // PS 5.1 的 `Out-File -Encoding utf8` 会写 BOM，JSON.parse 不认 —— 剥掉。
  return JSON.parse(readFileSync(join(FIX, 'monitor.json'), 'utf8').replace(/^\uFEFF/, ''));
}

const monitorPayload = loadFixtures();

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

/* =======================================================================
 * 一、主界面：实时监控页
 * ======================================================================= */
const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });

const pageErrors = [];
page.on('pageerror', (e) => pageErrors.push(String(e)));
page.on('console', (m) => { if (m.type() === 'error') pageErrors.push('console: ' + m.text()); });

/** 在页面脚本执行前接入 Tauri 的 invoke。
 *  注意：addInitScript 的 data 参数会被 JSON 序列化，**函数过不去**，
 *  所以应答逻辑必须写在 init 脚本体内，只把真机 payload 当数据传进来。 */
await page.addInitScript(({ monitorPayload }) => {
  const ok = (data) => ({ ok: true, data, error: null });
  const err = (msg) => ({ ok: false, data: null, error: msg });

  const calls = [];
  let snap = { ...monitorPayload, running: true };
  let fb = { visible: false, x: 1600, y: 800, size: 72, opacity: 0.92, collapsed: false };

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        calls.push({ cmd, args });
        switch (cmd) {
          /* 进程页：给份最小可用数据，别让 init() 崩 */
          case 'scan_processes':
            return ok({
              processes: [],
              metrics: { cpu_percent: 0, mem_used: 0, mem_total: 1, disk_percent: 0, disk_read_bps: 0, disk_write_bps: 0, net_rx_bps: 0, net_tx_bps: 0, process_count: 0, thread_count: 0, uptime_secs: 1 },
              stats: { process_count: 0, enumerate_ms: 0, detail_ms: 0, total_ms: 0 },
            });
          case 'env_info':
            return ok({ version: '0.1.3', cpu_count: 12, pdh_healthy: true, portable: true, data_dir: 'D:\\old666\\data', admin: true, blacklist_coverage: [237, 47, 46] });
          case 'whitelist_list': return ok({ names: [], paths: [] });
          case 'snapshot_list': return ok([]);

          /* ---- 监控 ---- */
          case 'monitor_snapshot': return ok(snap);
          case 'monitor_toggle':
            snap = { ...snap, running: !!args.enabled };
            return ok(snap);
          case 'monitor_configure':
            snap = { ...snap, config: { ...snap.config, ...args.config } };
            return ok(snap.config);
          case 'monitor_clear_alerts':
            snap = { ...snap, alerts: [] };
            return ok(0);

          /* ---- 悬浮球 ---- */
          case 'floatball_state': return ok(fb);
          case 'floatball_show':
            fb = { ...fb, visible: !!args.show };
            return ok(fb.visible);
          case 'floatball_save':
            fb = { ...fb, ...args.state };
            return ok(fb);

          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
  window.__TAURI_TEST__ = { calls };
}, { monitorPayload });

await page.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/index.html');
await page.waitForTimeout(400);

console.log('\n== 1. 进入实时监控 ==');
await page.click('.nav-item[data-view="monitor"]');
await page.waitForSelector('#viewMonitor:not([hidden])');
await page.waitForFunction(() => {
  const el = document.querySelector('#monCpu');
  return el && el.textContent && el.textContent !== '—';
});
await page.waitForTimeout(200);

check('视图切到实时监控', await page.isVisible('#viewMonitor'));
check('标题更新', (await page.textContent('#viewTitle')) === '实时监控');
check('进程页收起', await page.isHidden('#viewPurge'));
check('监控导航已启用（不再是 disabled）',
  await page.evaluate(() => !document.querySelector('.nav-item[data-view="monitor"]').disabled));
check('监控导航没有「P5」待办标记',
  await page.evaluate(() => !document.querySelector('.nav-item[data-view="monitor"] .soon')));

console.log('\n== 2. 指标卡片 ==');
const sm = monitorPayload.sample;
const shown = await page.evaluate(() => ({
  cpu: document.querySelector('#monCpu').textContent,
  mem: document.querySelector('#monMem').textContent,
  disk: document.querySelector('#monDisk').textContent,
  net: document.querySelector('#monNet').textContent,
  proc: document.querySelector('#monProc').textContent,
  selfMem: document.querySelector('#monSelfMem').textContent,
  selfCpu: document.querySelector('#monSelfCpu').textContent,
}));
check('CPU 与后端一致', shown.cpu === sm.cpu_percent.toFixed(0), `${shown.cpu} vs ${sm.cpu_percent}`);
check('内存带百分号且与后端一致', shown.mem === sm.mem_percent.toFixed(0) + '%', shown.mem);
check('磁盘占用与后端一致', shown.disk.replace('%', '') === sm.disk_percent.toFixed(0), shown.disk);
check('进程数与后端一致', shown.proc === String(sm.process_count), shown.proc);
check('自身内存带 MB 且与后端一致',
  shown.selfMem === sm.self_working_set_mb.toFixed(1) + ' MB', shown.selfMem);
check('网络速率带 /s 后缀', /\/s$/.test(shown.net), shown.net);
check('自身 CPU 有数值', /%$/.test(shown.selfCpu), shown.selfCpu);

console.log('\n== 3. 状态与开关 ==');
const sub = await page.textContent('#monSub');
check('说明文字写明正在后台采样', /后台采样/.test(sub), sub);
check('开关按钮文案 = 运行中', (await page.textContent('#btnMonToggle')) === '关闭常驻监控');
await page.click('#btnMonToggle');
await page.waitForTimeout(200);
check('关闭后按钮文案切换', (await page.textContent('#btnMonToggle')) === '开启常驻监控');
check('关闭后说明文字变了', /已关闭/.test(await page.textContent('#monSub')));
check('确实调了 monitor_toggle 且 enabled=false',
  await page.evaluate(() => window.__TAURI_TEST__.calls.some(
    (c) => c.cmd === 'monitor_toggle' && c.args.enabled === false)));
await page.click('#btnMonToggle');
await page.waitForTimeout(200);

console.log('\n== 4. 阈值表单 ==');
const th = await page.evaluate(() => ({
  cpu: document.querySelector('#monCpuTh').value,
  mem: document.querySelector('#monMemTh').value,
  disk: document.querySelector('#monDiskTh').value,
  interval: document.querySelector('#monInterval').value,
}));
check('CPU 阈值回填自后端', Number(th.cpu) === monitorPayload.config.cpu_threshold, th.cpu);
check('内存阈值回填自后端', Number(th.mem) === monitorPayload.config.mem_threshold, th.mem);
check('磁盘阈值回填自后端', Number(th.disk) === monitorPayload.config.disk_threshold, th.disk);
check('采样间隔按秒回填', Number(th.interval) === Math.round(monitorPayload.config.interval_ms / 1000), th.interval);

await page.fill('#monCpuTh', '70');
await page.click('#btnMonSave');
await page.waitForTimeout(200);
check('保存阈值调用了 monitor_configure',
  await page.evaluate(() => window.__TAURI_TEST__.calls.some(
    (c) => c.cmd === 'monitor_configure' && c.args.config.cpu_threshold === 70)));

console.log('\n== 5. 告警区 ==');
check('无告警时有明确的空态文案',
  /没有告警/.test(await page.textContent('#monAlerts')));
const hasClearBtn = await page.evaluate(() => !!document.querySelector('#btnMonClear'));
check('有「清空告警」按钮', hasClearBtn);

console.log('\n== 6. 悬浮球控件 ==');
check('悬浮球开关按钮存在', await page.evaluate(() => !!document.querySelector('#btnFbToggle')));
check('悬浮球尺寸输入存在', await page.evaluate(() => !!document.querySelector('#fbSize')));
check('悬浮球透明度输入存在', await page.evaluate(() => !!document.querySelector('#fbOpacity')));
await page.click('#btnFbToggle');
await page.waitForTimeout(200);
check('切换悬浮球调用了 floatball_show',
  await page.evaluate(() => window.__TAURI_TEST__.calls.some((c) => c.cmd === 'floatball_show')));

console.log('\n== 7. 红线：监控页不提供结束进程的入口 ==');
const killish = await page.evaluate(() => {
  const ids = [...document.querySelectorAll('#viewMonitor button, #viewMonitor a, #viewMonitor input')]
    .map((e) => (e.id || '') + ' ' + (e.textContent || '') + ' ' + (e.value || ''));
  return ids.filter((t) => /结束|结束进程|强制结束|清理进程|净化|kill|结束进程/.test(t));
});
check('监控页没有任何「结束进程」类按钮', killish.length === 0, killish.join(' | '));
const monitorCalls = await page.evaluate(() =>
  window.__TAURI_TEST__.calls.map((c) => c.cmd));
check('监控页只调用监控/悬浮球类命令（不含 purge/kill）',
  monitorCalls.every((c) => !/purge|kill|trim|startup_apply/.test(c)),
  [...new Set(monitorCalls)].join(','));

await page.screenshot({ path: join(OUT, 'monitor-view.png'), fullPage: true });
await page.close();

/* =======================================================================
 * 二、悬浮球：独立窗口页面
 * ======================================================================= */
console.log('\n== 8. 悬浮球独立页面 ==');
const fbPage = await browser.newPage({ viewport: { width: 320, height: 320 } });
const fbErrors = [];
fbPage.on('pageerror', (e) => fbErrors.push(String(e)));
fbPage.on('console', (m) => { if (m.type() === 'error') fbErrors.push('console: ' + m.text()); });

await fbPage.addInitScript(() => {
  const ok = (data) => ({ ok: true, data, error: null });
  const calls = [];
  let fb = { visible: true, x: 1600, y: 800, size: 72, opacity: 0.92, collapsed: false };
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        calls.push({ cmd, args });
        switch (cmd) {
          case 'floatball_state': return ok(fb);
          case 'floatball_save': fb = { ...fb, ...args.state }; return ok(fb);
          case 'floatball_show': fb = { ...fb, visible: !!args.show }; return ok(fb.visible);
          case 'monitor_snapshot': return ok({
            running: true,
            sample: {
              cpu_percent: 12.5, mem_percent: 63.2, disk_percent: 41.0,
              net_rx_bps: 1024, net_tx_bps: 2048, process_count: 321,
              self_working_set_mb: 30.5, self_cpu_percent: 0.4, interval_ms: 5000,
            },
            peak_cpu: 0, peak_mem: 0, peak_self_mem: 0, rounds: 3, alerts: [],
            config: { enabled: true, interval_ms: 5000, cpu_threshold: 85, mem_threshold: 90, disk_threshold: 95 },
          });
          case 'mode_set_active': return ok(args.modeId);
          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
  window.__TAURI_TEST__ = { calls };
});

await fbPage.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/floatball.html');
await fbPage.waitForTimeout(400);

check('悬浮球球体渲染出来', await fbPage.isVisible('#orb'));
check('有拖动手柄', await fbPage.isVisible('#ball'));
check('单击展开的面板存在', await fbPage.evaluate(() => !!document.querySelector('#panel')));
check('面板默认收起', !(await fbPage.evaluate(() => document.querySelector('#panel').classList.contains('show'))));

await fbPage.click('#orb');
await fbPage.waitForTimeout(300);
check('单击后面板展开',
  await fbPage.evaluate(() => document.querySelector('#panel').classList.contains('show')));
check('面板显示内存', /%/.test(await fbPage.textContent('#pMem')), await fbPage.textContent('#pMem'));
check('面板显示 CPU', /%/.test(await fbPage.textContent('#pCpu')), await fbPage.textContent('#pCpu'));
check('面板显示进程数', (await fbPage.textContent('#pProc')) === '321', await fbPage.textContent('#pProc'));

await fbPage.click('#pCollapse');
await fbPage.waitForTimeout(200);
check('「收起」按钮把球体切到收起态',
  (await fbPage.textContent('#orb')) === '小', await fbPage.textContent('#orb'));

/* 右键轮换模式 */
await fbPage.click('#orb', { button: 'right' });
await fbPage.waitForTimeout(300);
const modeCalls = await fbPage.evaluate(() =>
  window.__TAURI_TEST__.calls.filter((c) => c.cmd === 'mode_set_active').map((c) => c.args.modeId));
check('右键轮换到场景模式（daily→game）',
  modeCalls.length >= 1 && modeCalls[0] === 'game', JSON.stringify(modeCalls));
check('右键只切模式、不结束任何进程',
  await fbPage.evaluate(() => window.__TAURI_TEST__.calls.every(
    (c) => !/purge|kill|trim/.test(c.cmd))));

await fbPage.screenshot({ path: join(OUT, 'floatball-view.png'), fullPage: true });
await fbPage.close();

/* =======================================================================
 * 三、错误零容忍
 * ======================================================================= */
console.log('\n== 9. 无脚本错误 ==');
check('主界面零 pageerror', pageErrors.length === 0, pageErrors.slice(0, 3).join(' | '));
check('悬浮球零 pageerror', fbErrors.length === 0, fbErrors.slice(0, 3).join(' | '));

await browser.close();

const failed = results.filter((r) => !r.pass);
console.log(`\n========================================`);
console.log(`  通过 ${results.length - failed.length}/${results.length}`);
if (failed.length) {
  console.log('  失败项：');
  for (const f of failed) console.log(`   ✗ ${f.name}${f.detail ? '  — ' + f.detail : ''}`);
}
console.log(`========================================`);
process.exit(failed.length ? 1 : 0);
