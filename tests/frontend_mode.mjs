/**
 * 老六清净 old666 · 场景模式页 无头渲染验证（P3）
 * ------------------------------------------------------------
 * 为什么要这个脚本：
 *   场景模式这一页是**「可解释 + 不误伤」的最后一道界面关口**：
 *   规则 DSL → 引擎 → 计划 → 用户确认 → 执行，全链路都靠这一页串起来。
 *   字段名差一个字母、徽标判断写反、确认弹窗漏了 —— 都不会崩，只会
 *   「安静地做错事」。所以必须逐条断言。
 *
 * 做法（与 tests/frontend_boot.mjs 同构）：
 *   1. file:// 直接加载 src/index.html；
 *   2. 在页面脚本执行前注入 window.__TAURI__，invoke 用**真机导出的
 *      真实后端 payload** 逐个命令应答；
 *   3. 断言：模式卡渲染、「会结束进程」徽标、计划逐条可解释、
 *      条件未命中标灰、影响清单、结束进程必须弹确认框且取消后
 *      **一个进程都不结束**（killConfirmed 不为 true）、自动切换默认关闭、
 *      自定义模式保存走同一套校验。
 *   4. 全页截图留档。
 *
 * 数据来源（真机、只读导出，缺了就由本脚本自动跑 CLI 生成到 tests/_out/fixtures/）：
 *   old666-poc --modes --json                → modes.json
 *   old666-poc --modes --mode-plan daily --json  → mode-plan-daily.json
 *   old666-poc --modes --mode-plan game --json   → mode-plan-game.json
 *   old666-poc --modes --mode-plan server --json → mode-plan-server.json
 *
 * 运行方式：
 *   node tests/frontend_mode.mjs
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
  const names = ['modes.json', 'mode-plan-daily.json', 'mode-plan-game.json', 'mode-plan-server.json'];
  const missing = names.filter((f) => !existsSync(join(FIX, f)));
  if (missing.length) {
    throw new Error(
      `缺少 fixture：${missing.join('、')}\n` +
      '  请先导出（真机只读，几秒钟）：\n' +
      '    old666-poc --modes --json > tests/_out/fixtures/modes.json\n' +
      '    old666-poc --modes --mode-plan daily --json > tests/_out/fixtures/mode-plan-daily.json\n' +
      '    old666-poc --modes --mode-plan game --json > tests/_out/fixtures/mode-plan-game.json\n' +
      '    old666-poc --modes --mode-plan server --json > tests/_out/fixtures/mode-plan-server.json');
  }
  // PS 5.1 的 `Out-File -Encoding utf8` 会写 BOM，JSON.parse 不认 —— 剥掉。
  const read = (f) => JSON.parse(
    readFileSync(join(FIX, f), 'utf8').replace(/^\uFEFF/, ''));
  return {
    modes: read('modes.json'),
    plans: {
      daily: read('mode-plan-daily.json'),
      game: read('mode-plan-game.json'),
      server: read('mode-plan-server.json'),
    },
  };
}

const { modes: modesPayload, plans } = loadFixtures();

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
await page.addInitScript(({ modesPayload, plans }) => {
  const ok = (data) => ({ ok: true, data, error: null });
  const err = (msg) => ({ ok: false, data: null, error: msg });

  /* 记录每一次 apply 的入参 —— 「用户没点头时会不会真的结束进程」
     就看这里：killConfirmed 必须是 false。 */
  const applies = [];
  let autoSwitch = modesPayload.auto_switch;
  const savedCustom = [];

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

          /* ---- 场景模式 ---- */
          case 'mode_list':
            return ok({ ...modesPayload, auto_switch: autoSwitch });
          case 'mode_plan': {
            const p = plans[args.modeId];
            if (!p) return err('没有找到模式「' + args.modeId + '」');
            return ok(p);
          }
          case 'mode_apply': {
            applies.push({
              modeId: args.modeId,
              ruleIds: args.ruleIds,
              killConfirmed: args.killConfirmed,
            });
            /* 真实后端：未确认时结束进程的动作会被跳过并在结果里说明原因。 */
            const plan = plans[args.modeId] || { mode_id: args.modeId, mode_name: args.modeId, rules: [] };
            const actions = (plan.rules || []).map((r) => ({
              rule_id: r.id,
              action: r.action,
              ok: !r.triggered,
              detail: r.triggered
                ? (r.action === 'purge_processes' && !args.killConfirmed
                    ? '会结束进程的动作需要你逐次确认，本次未执行任何结束操作'
                    : '已执行')
                : '条件未满足',
              freed_bytes: 0,
              skipped: !r.triggered || (r.action === 'purge_processes' && !args.killConfirmed),
            }));
            return ok({
              mode_id: plan.mode_id,
              mode_name: plan.mode_name,
              actions,
              freed_bytes: 0,
              summary: '已执行 0 项，释放 0 B',
            });
          }
          case 'mode_set_active': return ok(args.modeId);
          case 'mode_auto_get': return ok(autoSwitch);
          case 'mode_auto_set': {
            autoSwitch = {
              enabled: !!args.enabled,
              mem_above_pct: args.memAbovePct ?? null,
              mem_above_mode: args.memAboveMode ?? null,
              foreground_process: args.foregroundProcess ?? null,
              foreground_mode: args.foregroundMode ?? null,
            };
            return ok(autoSwitch);
          }
          case 'mode_custom_save': {
            let m;
            try { m = JSON.parse(args.json); } catch { return err('模式文件解析失败'); }
            if (!m.id || !m.name || !Array.isArray(m.rules) || !m.rules.length) {
              return err('模式 id / 名称 / rules 不能为空');
            }
            savedCustom.push(m);
            return ok(m.id);
          }
          case 'mode_custom_delete': return ok(args.modeId);

          default: throw new Error('未实现的命令: ' + cmd);
        }
      },
    },
  };
  window.__TAURI_TEST__ = { applies, savedCustom };
}, { modesPayload, plans });

await page.goto('file:///' + SRC.replace(/\\/g, '/').replace(/ /g, '%20') + '/index.html');
await page.waitForTimeout(400);

console.log('\n== 1. 进入场景模式 ==');
await page.click('.nav-item[data-view="mode"]');
await page.waitForSelector('#viewMode:not([hidden])');
await page.waitForFunction(() => document.querySelectorAll('#modeGrid .mode-card').length > 0);
await page.waitForTimeout(200);

check('视图切到场景模式', await page.isVisible('#viewMode'));
check('标题更新', (await page.textContent('#viewTitle')) === '场景模式');
check('进程页收起', await page.isHidden('#viewPurge'));
check('场景模式导航已启用（不再是 disabled）',
  await page.evaluate(() => !document.querySelector('.nav-item[data-view="mode"]').disabled));
check('场景模式导航没有「P3」待办标记',
  await page.evaluate(() => !document.querySelector('.nav-item[data-view="mode"] .soon')));

console.log('\n== 2. 模式卡 ==');
const cards = await page.$$eval('#modeGrid .mode-card', (els) =>
  els.map((e) => ({
    id: e.dataset.mode,
    name: e.querySelector('.mode-card-title')?.textContent || '',
    badges: [...e.querySelectorAll('.mode-badge')].map((b) => b.textContent),
  })));
check('模式卡数量 = 后端返回的模式数', cards.length === modesPayload.modes.length,
  `${cards.length} vs ${modesPayload.modes.length}`);
check('每张卡都有名字', cards.every((c) => c.name.length > 0));
check('内置模式带「内置」徽标', cards.filter((c) => c.badges.includes('内置')).length >= 3,
  cards.map((c) => `${c.id}:${c.badges.join('/')}`).join(' '));

/* 「会结束进程」的徽标必须由规则本身推出来 —— 判错方向会误导用户，
   漏标更糟：用户以为不会结束进程，结果会。 */
const killModeIds = modesPayload.modes
  .filter((m) => (m.rules || []).some((r) => r.then && r.then.action === 'purge_processes'))
  .map((m) => m.id);
const killBadges = cards.filter((c) => c.badges.includes('会结束进程')).map((c) => c.id);
check('「会结束进程」徽标标在正确的模式上',
  JSON.stringify(killBadges.slice().sort()) === JSON.stringify(killModeIds.slice().sort()),
  `标注=${killBadges.join(',')} 实际=${killModeIds.join(',')}`);

const activeName = await page.textContent('#modeActiveName');
check('当前生效模式显示正确', activeName === (modesPayload.modes.find((m) => m.id === modesPayload.active)?.name || ''),
  activeName);

console.log('\n== 3. 计划（先看计划，再动手） ==');
await page.click('#modeGrid .mode-card[data-mode="daily"]');
await page.waitForSelector('#modePlanCard:not([hidden])');
await page.waitForFunction(() => document.querySelectorAll('#modeRuleList .mode-rule').length > 0);
await page.waitForTimeout(150);

const dailyPlan = plans.daily;
const ruleEls = await page.$$eval('#modeRuleList .mode-rule', (els) =>
  els.map((e) => ({
    text: e.textContent,
    off: e.classList.contains('off'),
    hasSkip: !!e.querySelector('.mode-rule-skip'),
    hasWarn: !!e.querySelector('.mode-rule-warn'),
    targets: [...e.querySelectorAll('.mode-rule-targets li')].map((l) => l.textContent),
  })));
check('计划条数 = 规则数', ruleEls.length === dailyPlan.rules.length,
  `${ruleEls.length} vs ${dailyPlan.rules.length}`);
check('每条规则都渲染了标题', ruleEls.every((r) => r.text.length > 0));

/* 条件未满足的规则必须标灰并写明「条件未满足」—— 否则用户会以为它也会执行。 */
const expectedOff = dailyPlan.rules.filter((r) => !r.triggered);
const offOk = expectedOff.every((r) => {
  const el = ruleEls.find((x) => x.text.includes(r.title));
  return el && el.off && el.hasSkip;
});
check('条件未满足的规则被标灰并注明', offOk,
  `未命中 ${expectedOff.length} 条`);

/* 需要二次确认的规则必须有醒目标记。 */
const needConfirm = dailyPlan.rules.filter((r) => r.needs_confirm);
check('需确认的规则带「需确认」标记',
  needConfirm.length === 0 || needConfirm.every((r) => {
    const el = ruleEls.find((x) => x.text.includes(r.title));
    return el && el.hasWarn;
  }),
  `需确认 ${needConfirm.length} 条`);

/* delay_startup 的目标必须展开成具体条目名 —— 用户要看到「会动哪些东西」。 */
const delayRule = dailyPlan.rules.find((r) => r.action === 'delay_startup');
if (delayRule && delayRule.targets.length) {
  const el = ruleEls.find((x) => x.text.includes(delayRule.title));
  check('延迟启动的影响清单已展开',
    el && el.targets.length === delayRule.targets.length,
    `${el?.targets.length} vs ${delayRule.targets.length}`);
} else {
  check('延迟启动的影响清单已展开', true, '本次无匹配目标（跳过）');
}

console.log('\n== 4. 结束进程必须逐次确认 ==');
/* 用 server 模式 —— 它的规则里含 purge_processes，且要求二次确认。 */
await page.click('#modeGrid .mode-card[data-mode="server"]');
await page.waitForFunction((n) =>
  document.querySelectorAll('#modeRuleList .mode-rule').length === n,
  plans.server.rules.length);
await page.waitForTimeout(150);

const serverKilling = plans.server.rules.filter((r) => r.triggered && r.action === 'purge_processes');
if (serverKilling.length) {
  await page.click('#btnModeApply');
  await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
  check('执行前弹出确认框', await page.isVisible('#confirmMask'));

  const confirmText = await page.textContent('#confirmMask');
  check('确认框写明「会结束进程」', /会结束进程/.test(confirmText), confirmText.slice(0, 120));
  check('确认框列出受影响的规则',
    serverKilling.every((r) => confirmText.includes(r.title)));

  /* 关键断言：点「取消」之后，apply 根本没被调用。 */
  await page.click('#confirmCancel');
  await page.waitForTimeout(300);
  const appliedAfterCancel = await page.evaluate(() => window.__TAURI_TEST__.applies.length);
  check('点取消后没有调用 mode_apply（一个进程都没结束）', appliedAfterCancel === 0,
    `applies=${appliedAfterCancel}`);

  /* 再来一次，这次点头 —— 这时 killConfirmed 才允许为 true。 */
  await page.click('#btnModeApply');
  await page.waitForSelector('#confirmMask:not([hidden])', { timeout: 8000 });
  await page.click('#confirmOk');
  await page.waitForTimeout(400);
  const applies = await page.evaluate(() => window.__TAURI_TEST__.applies);
  check('点确认后才调用 mode_apply', applies.length === 1, `applies=${applies.length}`);
  check('确认后 killConfirmed 为 true',
    applies.length === 1 && applies[0].killConfirmed === true,
    JSON.stringify(applies[0] || {}));
} else {
  check('执行前弹出确认框', true, '本机内存未达阈值，无结束动作（跳过）');
}

console.log('\n== 5. 自动切换默认关闭 ==');
check('自动切换默认是关的',
  await page.evaluate(() => document.querySelector('#modeAutoEnabled').value === '0'));
check('自动切换按钮文案显示「关」',
  /关/.test(await page.textContent('#btnModeAuto')),
  await page.textContent('#btnModeAuto'));
check('后端默认配置也是关的',
  modesPayload.auto_switch.enabled === false,
  JSON.stringify(modesPayload.auto_switch));

await page.click('#btnModeAuto');
await page.waitForTimeout(150);
check('点开后出现自动切换设置卡', await page.isVisible('#modeAutoCard'));

console.log('\n== 6. 自定义模式 ==');
await page.click('#btnModeCustom');
await page.waitForSelector('#modeMask:not([hidden])');
const tpl = await page.inputValue('#modeJson');
check('编辑器预填了模板 JSON', /"rules"/.test(tpl) && /"id"/.test(tpl), tpl.slice(0, 60));

await page.fill('#modeJson', '{"id":"broken"}');
await page.click('#modeModalSave');
await page.waitForTimeout(300);
check('不合法的模式被拒绝（弹窗保持打开）', await page.isVisible('#modeMask'));

await page.fill('#modeJson', '{"id":"my_mode","name":"我的模式","rules":[{"id":"r1","title":"裁剪","when":{"kind":"always"},"then":{"action":"trim_memory"}}]}');
await page.click('#modeModalSave');
await page.waitForTimeout(300);
const saved = await page.evaluate(() => window.__TAURI_TEST__.savedCustom);
check('合法的自定义模式保存成功', saved.length === 1 && saved[0].id === 'my_mode',
  JSON.stringify(saved.map((s) => s.id)));

console.log('\n== 7. 截图 ==');
await page.evaluate(() => {
  const v = document.querySelector('#viewMode');
  if (v) v.scrollTop = 0;
  window.scrollTo(0, 0);
});
await page.screenshot({ path: join(OUT, 'mode-view.png'), fullPage: true });
console.log(`截图：${join(OUT, 'mode-view.png')}`);

console.log('\n== 8. 无 JS 报错 ==');
check('零 pageerror / console.error', pageErrors.length === 0, pageErrors.slice(0, 3).join(' | '));

await browser.close();

const failed = results.filter((r) => !r.pass);
console.log(`\n================ 结果：${results.length - failed.length}/${results.length} 通过 ================`);
if (failed.length) {
  console.log('未通过：');
  for (const f of failed) console.log('  - ' + f.name + (f.detail ? ' (' + f.detail + ')' : ''));
  process.exit(1);
}
