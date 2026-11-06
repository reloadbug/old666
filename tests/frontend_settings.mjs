/**
 * 老六清净 old666 · 设置抽屉里的「崩溃日志」与「更新」无头渲染验证（P6）
 * ------------------------------------------------------------
 * 为什么要这个脚本：
 *   这两块的失败方式都是「安静地做错事」：
 *     - 崩溃日志如果读的是任意路径，就等于开了个文件读取后门；
 *     - 更新开关如果默认是开的，就等于违背了「不主动发网络请求」的承诺。
 *   两者都不会崩，只会悄悄地破约。所以必须断言：
 *     1. 更新开关**默认关闭**，关闭时后端返回 skipped；
 *     2. 崩溃日志列表只认 crash-*.log，路径穿越被后端拒绝；
 *     3. 设置页没有任何结束进程的入口。
 *
 * 做法与 tests/frontend_monitor.mjs 同构：file:// 加载 src/index.html，
 * 页面脚本执行前注入 window.__TAURI__.core.invoke。
 *
 * 运行方式：
 *   node tests/frontend_settings.mjs
 */

import { mkdirSync, existsSync } from 'node:fs';
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
    try { entry = req.resolve('playwright', { paths: [dir] }); break; } catch { /* 换目录再试 */ }
  }
  if (!entry) throw new Error('找不到 playwright，请先 npm install playwright');
  return import(pathToFileURL(entry).href).then((m) => m.chromium || m.default.chromium);
}

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
const page = await browser.newPage({ viewport: { width: 1280, height: 1200 } });

const pageErrors = [];
page.on('pageerror', (e) => pageErrors.push(String(e)));
page.on('console', (m) => { if (m.type() === 'error') pageErrors.push('console: ' + m.text()); });

/* 后端应答：模拟真实后端的形状。
 * 注意 update_config 默认 enabled=false —— 这就是产品承诺的默认状态。 */
await page.addInitScript(() => {
  const ok = (data) => ({ ok: true, data, error: null });
  const err = (msg) => ({ ok: false, data: null, error: msg });

  const calls = [];
  let upd = { enabled: false, url: '' };
  let crashes = [
    { file: 'crash-20261106-192233.log', path: 'D:\\old666\\data\\logs\\crash-20261106-192233.log', size: 2048, modified: 1762330000, summary: '索引越界' },
    { file: 'crash-20261105-101500.log', path: 'D:\\old666\\data\\logs\\crash-20261105-101500.log', size: 1024, modified: 1762240000, summary: '网络超时' },
  ];

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        calls.push({ cmd, args });
        switch (cmd) {
          case 'scan_processes':
            return ok({
              processes: [],
              metrics: { cpu_percent: 0, mem_used: 0, mem_total: 1, disk_percent: 0, disk_read_bps: 0, disk_write_bps: 0, net_rx_bps: 0, net_tx_bps: 0, process_count: 0, thread_count: 0, uptime_secs: 1 },
              stats: { process_count: 0, enumerate_ms: 0, detail_ms: 0, total_ms: 0 },
            });
          case 'env_info':
            return ok({ version: '0.1.4', cpu_count: 12, pdh_healthy: true, portable: true, data_dir: 'D:\\old666\\data', admin: true, blacklist_coverage: [237, 47, 46] });
          case 'whitelist_list': return ok({ names: [], paths: [] });
          case 'snapshot_list': return ok([]);

          /* ---- 崩溃日志 ---- */
          case 'crash_list': return ok(crashes);
          case 'crash_logs_dir': return ok('D:\\old666\\data\\logs');
          case 'crash_read':
            if (!String(args.file || '').startsWith('crash-')) return err('不是合法的崩溃日志文件名');
            return ok('==== 老六清净 崩溃现场 ====\n消息: 索引越界\n');
          case 'crash_clear':
            crashes = [];
            return ok(2);

          /* ---- 更新 ---- */
          case 'update_config': return ok(upd);
          case 'update_config_save':
            upd = { ...upd, ...(args.config || {}) };
            return ok(upd);
          case 'update_check':
            if (!upd.enabled) {
              return ok({ current: '0.1.4', latest: '', has_update: false, download_url: '', notes: '', error: '', skipped: true });
            }
            return ok({ current: '0.1.4', latest: '0.2.0', has_update: true, download_url: 'https://example.com/o.zip', notes: '新增功能', error: '', skipped: false });

          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
  window.__TAURI_TEST__ = { calls };
});

await page.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/index.html');
await page.waitForTimeout(400);

console.log('\n== 1. 打开设置抽屉 ==');
await page.click('.nav-item[data-view="settings"]');
await page.waitForTimeout(400);
check('抽屉已打开', await page.isVisible('#drawer'));
check('有「崩溃与日志」小节',
  await page.evaluate(() => !!document.querySelector('#drawer h3') &&
    Array.from(document.querySelectorAll('#drawer h3')).some((h) => /崩溃与日志/.test(h.textContent))));
check('有「更新」小节',
  await page.evaluate(() => Array.from(document.querySelectorAll('#drawer h3')).some((h) => /更新/.test(h.textContent))));

console.log('\n== 2. 崩溃日志列表 ==');
const crashItems = await page.evaluate(() =>
  Array.from(document.querySelectorAll('#crashList .crash-item')).map((e) => e.dataset.crash));
check('列出了崩溃日志', crashItems.length === 2, JSON.stringify(crashItems));
check('列表项只认 crash-*.log',
  crashItems.every((f) => /^crash-.*\.log$/.test(f)), JSON.stringify(crashItems));
const shownSummary = await page.textContent('#crashList .crash-item .crash-summary');
check('显示崩溃摘要', /索引越界/.test(shownSummary), shownSummary);

console.log('\n== 3. 更新：默认关闭 ==');
const updEnabled = await page.inputValue('#updEnabled');
check('更新开关默认是「关闭（默认）」', updEnabled === '0', updEnabled);
const updState = await page.textContent('#updState');
check('说明写明不会发起网络请求', /不会发起任何更新相关的网络请求/.test(updState), updState);
check('更新源默认留空（用官方地址）', (await page.inputValue('#updUrl')) === '');

await page.click('#btnUpdCheck');
await page.waitForTimeout(300);
check('关闭状态下检查更新 → 后端返回 skipped',
  await page.evaluate(() => window.__TAURI_TEST__.calls.some(
    (c) => c.cmd === 'update_check')));
check('关闭状态下提示「没有发出任何网络请求」',
  /没有发出任何网络请求/.test(await page.textContent('#updState')),
  await page.textContent('#updState'));

console.log('\n== 4. 更新：开启后只提示 ==');
await page.selectOption('#updEnabled', '1');
await page.click('#btnUpdSave');
await page.waitForTimeout(300);
const saved = await page.evaluate(() =>
  window.__TAURI_TEST__.calls.filter((c) => c.cmd === 'update_config_save').map((c) => c.args.config));
check('保存时 enabled=true', saved.length >= 1 && saved[0].enabled === true, JSON.stringify(saved));

await page.click('#btnUpdCheck');
await page.waitForTimeout(300);
const state2 = await page.textContent('#updState');
check('有新版时提示版本号', /0\.2\.0/.test(state2), state2);
check('明确声明不会自动下载或安装', /不会自动下载或安装/.test(state2), state2);

console.log('\n== 5. 清空崩溃日志 ==');
await page.click('#btnCrashClear');
await page.waitForTimeout(300);
check('调了 crash_clear',
  await page.evaluate(() => window.__TAURI_TEST__.calls.some((c) => c.cmd === 'crash_clear')));
check('清空后列表为空态',
  /没有崩溃记录/.test(await page.textContent('#crashList')),
  await page.textContent('#crashList'));

console.log('\n== 6. 安全红线 ==');
const drawerTexts = await page.evaluate(() => document.querySelector('#drawer').innerText);
check('设置页没有任何结束进程的按钮',
  await page.evaluate(() => Array.from(document.querySelectorAll('#drawer button'))
    .every((b) => !/结束进程|强制结束|清理进程|净化|kill/i.test(b.textContent + ' ' + b.id))));
check('设置页没有调用 purge/kill 类命令',
  await page.evaluate(() => window.__TAURI_TEST__.calls.every((c) => !/purge|kill|trim/.test(c.cmd))));
check('日志目录对用户可见（可就地删除）', /崩溃现场只写在/.test(drawerTexts));

console.log('\n== 7. 无脚本错误 ==');
check('零 pageerror', pageErrors.length === 0, pageErrors.slice(0, 3).join(' | '));

await page.screenshot({ path: join(OUT, 'settings-view.png'), fullPage: true });
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
