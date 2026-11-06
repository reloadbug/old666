/* ============================================================
   老六清净 old666 · 开机加速视图（P2）
   ------------------------------------------------------------
   设计原则（与后端一一对应）：
   1. **先看到东西，再补细节** —— 首屏只调 `startup_scan`（快速、不验签），
      拿到列表后再调 `startup_verify` 补签名。后端实测 154 ms vs 2.3 s。
   2. **禁用前必须出示清单** —— 点「禁用选中项」先跑 `startup_plan` 拿演练结果，
      把「会动哪些、怎么还原」逐条摆出来，用户点头后才调 `startup_apply`。
   3. **浏览器预览不造假** —— 没有 Tauri 时诚实地说明本页需要真实环境，
      绝不渲染一份编造的开机时间线。
   ============================================================ */

/* 这里刻意不访问 main.js 里的 `$` / `invoke`：
   启动顺序不固定，自己持一份引用最稳。 */
const $b = (s) => document.querySelector(s);
const bInvoke = (typeof window !== 'undefined' && window.__TAURI__ && window.__TAURI__.core)
  ? window.__TAURI__.core.invoke
  : null;

/* ---------------------- 本地状态 ---------------------- */
const bootState = {
  timeline: null,
  items: [],
  srcFilter: 'all',
  search: '',
  picked: new Set(),
  verifying: false,
  /** 隐藏「已禁用」的行（系统里本来就关着的那些，占列表一多半）。 */
  hideDisabled: readHidePref(),
  /** 最近一次扫描的元信息（副标题要用：耗时 / 待补签数 / 是否需要管理员）。 */
  lastScan: null,
  /** 当前打开「说明」面板的那一条。 */
  info: null,
  /** 「说明」面板是否处于编辑态。 */
  editing: false,
};

/** 「隐藏已禁用」是个习惯性开关：记在本地，下次打开还是用户上次选的。 */
function readHidePref() {
  try {
    return window.localStorage.getItem('old666.hideDisabled') === '1';
  } catch {
    return false; // file:// 或隐私模式下 localStorage 可能不可用
  }
}

function saveHidePref(on) {
  try {
    window.localStorage.setItem('old666.hideDisabled', on ? '1' : '0');
  } catch { /* 存不了就算了，只是记不住偏好 */ }
}

/* ---------------------- 常量 ---------------------- */

/** 分段配色。都是饱和色，白字可读；空段由后端按 key 返回，缺省退灰。 */
const PHASE_COLORS = {
  loader: '#6366F1',
  kernel: '#8B5CF6',
  drivers: '#0EA5A4',
  devices: '#64748B',
  services: '#F59E0B',
  smss: '#94A3B8',
  machine_profile: '#A1A1AA',
  profile: '#14B8A6',
  explorer: '#22C55E',
  other_main: '#94A3B8',
  postboot: '#3B82F6',
};

const RISK_LABELS = {
  safe: '可安全禁用',
  caution: '谨慎处理',
  dangerous: '不建议禁用',
  unknown: '无法判定',
};

const SRC_LABELS = {
  run_key: '注册表',
  startup_folder: '启动文件夹',
  service: '自动服务',
  scheduled_task: '计划任务',
};

/** 建议文案 → 面板上的配色档位。 */
const ADVICE_CLASS = {
  '可以禁用': '',
  '建议禁用': '',
  '谨慎处理': 'keep',
  '建议保留': 'keep',
  '暂无建议': 'keep',
};

/* ---------------------- 工具 ---------------------- */
function bHumanMs(ms) {
  const n = Number(ms || 0);
  if (n >= 1000) return (n / 1000).toFixed(1) + ' s';
  return Math.round(n) + ' ms';
}

function bEsc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

/** 轻量提示。刻意不复用 main.js 的 toast —— 脚本加载顺序不固定，
 *  自己持一份才不会出现「点击时 toast 还没定义」。 */
function bToast(msg, isErr) {
  const fn = (typeof window !== 'undefined' && typeof toast === 'function') ? toast : null;
  if (fn) { fn(msg, isErr); return; }
  const el = document.getElementById('toast');
  if (!el) { alert(msg); return; }
  el.textContent = msg;
  el.className = 'toast' + (isErr ? ' err' : '');
  el.hidden = false;
  clearTimeout(el._bt);
  el._bt = setTimeout(() => { el.hidden = true; }, 2800);
}

/** 统一解包后端 `Reply<T>`（`{ok, data, error}`）的结果。
 *
 *  后端 P2 命令一律返回 `Reply`，整体 **resolve** 而不是 reject —— 安全闸
 *  拦下的拒绝（演练模式、不在白名单、条目为空…）也走这条路。所以必须在
 *  这里把 `ok:false` 转成异常，各调用点已有的 try/catch 才能把原因显示
 *  给用户，而不是让页面拿着一份空数据继续渲染。
 *
 *  判别 `Reply` 用 `typeof ok === 'boolean'`：还原结果 `RollbackOutcome`
 *  里也有个 `ok`，但它是数字（成功条数），不能混为一谈。
 */
async function bCall(cmd, args) {
  if (!bInvoke) throw new Error('no-tauri');
  const r = await bInvoke(cmd, args);
  if (r && typeof r === 'object' && typeof r.ok === 'boolean' && 'data' in r) {
    if (!r.ok) throw new Error(r.error || '操作失败');
    return r.data;
  }
  return r;
}

/* 把 ISO 时间压成人话（不带年份，省地方） */
function bFmtTime(iso) {
  if (!iso) return '时间未知';
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  const p = (v) => String(v).padStart(2, '0');
  return `${d.getMonth() + 1}月${d.getDate()}日 ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/* ============================================================
   开机时间线
   ============================================================ */
async function loadBoot() {
  if (!bInvoke) {
    $b('#bootNow').textContent = '预览模式';
    $b('#bootCompare').textContent = '此页需要真实系统环境';
    const note = $b('#bootNote');
    note.hidden = false;
    note.textContent = '当前在浏览器里预览，读取不到真实开机数据。请运行「cargo run --release --bin old666」查看本页效果。';
    return;
  }
  try {
    bootState.timeline = await bCall('boot_timeline', { limit: 8 });
  } catch (e) {
    $b('#bootNow').textContent = '—';
    const note = $b('#bootNote');
    note.hidden = false;
    note.textContent = '读取开机数据失败：' + e;
    return;
  }
  renderBoot();
}

function renderBoot() {
  const tl = bootState.timeline;
  if (!tl || !tl.available) {
    $b('#bootNow').textContent = '—';
    $b('#bootBar').innerHTML = '';
    $b('#bootLegend').innerHTML = '';
    $b('#trendChart').innerHTML = '';
    $b('#trendCount').textContent = '0';
    const note = $b('#bootNote');
    note.hidden = false;
    note.textContent = (tl && tl.note) || '暂无开机记录';
    $b('#culpritCard').hidden = true;
    return;
  }

  const cur = tl.records[0];
  const delta = tl.avg_ms ? cur.boot_ms - tl.avg_ms : 0;

  $b('#bootNow').textContent = bHumanMs(cur.boot_ms);
  $b('#bootCompare').innerHTML = delta === 0
    ? `最近 ${tl.records.length} 次均值 ${bHumanMs(tl.avg_ms)}`
    : `${delta > 0 ? '比均值慢' : '比均值快'} ${bHumanMs(Math.abs(delta))}`;

  $b('#trendCount').textContent = String(tl.records.length);
  $b('#bootSub').textContent =
    `数据来自系统自带的开机性能诊断日志 · 最快 ${bHumanMs(tl.best_ms)} / 最慢 ${bHumanMs(tl.worst_ms)}`;

  renderBootBar(cur);
  renderTrend(tl);
  renderCulprits(cur);
}

function renderBootBar(rec) {
  const bar = $b('#bootBar');
  const total = rec.phases.reduce((s, p) => s + p.ms, 0) || 1;
  const legend = $b('#bootLegend');
  bar.innerHTML = '';
  legend.innerHTML = '';

  for (const ph of rec.phases) {
    const pct = (ph.ms / total) * 100;
    const color = PHASE_COLORS[ph.key] || '#94A3B8';
    const seg = document.createElement('div');
    seg.className = 'seg' + (pct < 6 ? ' narrow' : '');
    seg.style.width = pct + '%';
    seg.style.background = color;
    seg.title = `${ph.label}：${bHumanMs(ph.ms)}（${pct.toFixed(1)}%）${ph.actionable ? ' · 可优化' : ''}`;
    if (pct >= 6) seg.textContent = bHumanMs(ph.ms);
    bar.appendChild(seg);

    const lg = document.createElement('div');
    lg.className = 'lg';
    lg.innerHTML =
      `<i class="sw" style="background:${color}"></i>` +
      `<span>${bEsc(ph.label)}</span>` +
      `<b>${bHumanMs(ph.ms)}</b>` +
      (ph.actionable ? '<em>可优化</em>' : '');
    lg.title = ph.actionable
      ? '这一段的耗时可以通过下面的启动项管理来缩短'
      : '这一段由系统自身开销决定，用户无法干预';
    legend.appendChild(lg);
  }

  const eq = document.createElement('div');
  eq.className = 'lg';
  eq.innerHTML = `<b>合计 ${bHumanMs(rec.boot_ms)}</b>` +
    `<span>（主路径 ${bHumanMs(rec.main_path_ms)} + 桌面后台 ${bHumanMs(rec.post_boot_ms)}）</span>`;
  legend.appendChild(eq);
}

function renderTrend(tl) {
  const box = $b('#trendChart');
  box.innerHTML = '';
  // 柱状图按时间正序画：最旧在左、最新在右
  const recs = [...tl.records].reverse();
  const max = Math.max.apply(null, recs.map((r) => r.boot_ms).concat([1]));

  for (const r of recs) {
    const h = Math.max(4, Math.round((r.boot_ms / max) * 100));
    const bar = document.createElement('div');
    bar.className = 'bar';
    bar.title = `${bFmtTime(r.start_time)}\n开机 ${bHumanMs(r.boot_ms)}（主路径 ${bHumanMs(r.main_path_ms)}）`;
    bar.innerHTML = `<i style="height:${h}%"></i><span>${bHumanMs(r.boot_ms)}</span>`;
    box.appendChild(bar);
  }
}

function renderCulprits(rec) {
  const card = $b('#culpritCard');
  const list = $b('#culpritList');
  list.innerHTML = '';
  if (!rec.culprits || !rec.culprits.length) {
    card.hidden = true;
    return;
  }
  card.hidden = false;
  for (const c of rec.culprits) {
    const li = document.createElement('li');
    const company = c.company ? ` · ${bEsc(c.company)}` : '';
    const friendly = c.friendly_name && c.friendly_name !== c.name
      ? `${bEsc(c.friendly_name)} · ` : '';
    li.innerHTML =
      `<span class="k ${bEsc(c.kind)}">${bEsc(c.kind)}</span>` +
      `<span class="nm" title="${bEsc(friendly)}${bEsc(c.name)}${company}\n路径：${bEsc(c.path)}">` +
      `${bEsc(c.name)}</span>` +
      `<span class="tm"><span>共 ${bHumanMs(c.total_ms)}</span>` +
      `<b>拖慢 ${bHumanMs(c.degradation_ms)}</b></span>`;
    list.appendChild(li);
  }
}

/* ============================================================
   启动项
   ============================================================ */
async function loadStartup() {
  if (!bInvoke) {
    $b('#startupSub').textContent = '预览模式：需要真实系统环境';
    return;
  }
  $b('#startupSub').textContent = '正在扫描…';
  $b('#startupBody').innerHTML = '';
  bootState.picked.clear();
  syncDisableBtn();

  try {
    const scan = await bCall('startup_scan');
    // 后端已把开机归因回填到 impact_ms（IPC 层做，~2.6 ms）
    bootState.items = scan.items || [];
    // 副标题统一由 updateStartupSub 决定：还有待补签的条目就显示「正在确认」，
    // 别在这里再直接赋值一遍，否则刚渲染出来的提示会被下一行盖掉。
    renderStartup(scan);
    syncStartupCounts(scan.counts || []);
    // 首屏已渲染，现在补签名 —— 这两步分开就是为了让列表先出现
    verifyStartup();
  } catch (e) {
    $b('#startupSub').textContent = '扫描失败：' + e;
  }
}

/** 只补「当前可见」的签名，避免为折叠起来的大文件白读几百 MB。
 *
 *  没有筛选时可见集合就是全部，等价于一次全量补签（实测 ~0.7 s）；
 *  用户一旦筛到某个来源，就只补那一部分 —— 首屏体感明显更快。
 */
async function verifyStartup() {
  if (bootState.verifying) return;
  const ids = visibleStartups().map((i) => i.id);
  if (!ids.length) return;
  bootState.verifying = true;
  try {
    const res = await bCall('startup_verify', { ids });
    if (res && res.items) {
      bootState.items = res.items;
      renderStartup(res);
      syncStartupCounts(res.counts || []);
    }
  } catch (e) {
    // 补签失败不该打扰用户：列表已经能看，只是发布者还是「确认中…」
    console.warn('[old666] 补签失败', e);
  } finally {
    bootState.verifying = false;
  }
}

/** 列表里的档位：数字越小越靠前。
 *
 *  用户打开这一页就想知道「该关哪些」，所以**推荐禁用的排最前**；
 *  其次是其他能勾的（谨慎档）；再次是不建议禁用的（只能看）；
 *  已禁用的垫底 —— 它们是「已经处理过的」，不该跟待办混在一起。
 */
function displayRank(it) {
  if (recommended(it)) return 0;
  if (pickable(it)) return 1;
  if (it.enabled) return 2;
  return 3;
}

function visibleStartups() {
  const q = bootState.search.trim().toLowerCase();
  const out = bootState.items.filter((it) => {
    if (bootState.hideDisabled && !it.enabled) return false;
    if (bootState.srcFilter !== 'all' && it.source !== bootState.srcFilter) return false;
    if (!q) return true;
    return (it.name || '').toLowerCase().includes(q)
      || (it.command || '').toLowerCase().includes(q);
  });
  // 带下标的稳定排序：同档内保持后端给的顺序（来源 + 名称），
  // 不会因为每次渲染的比较结果相同却顺序抖动。
  return out
    .map((it, i) => ({ it, i, rank: displayRank(it) }))
    .sort((a, b) => a.rank - b.rank || a.i - b.i)
    .map((x) => x.it);
}

/** 能不能被勾选（= 能不能交给「禁用选中项」处理）。
 *
 *  已启用的条目才谈得上禁用；「不建议禁用」的默认不给勾，
 *  但如果用户已经把它加进黑名单，就按他的意思放开 ——
 *  黑名单是「这个我要关，别再问我」的明确表态。 */
function pickable(it) {
  return !!it.enabled && (it.can_disable || it.in_blacklist);
}

/** 推荐禁用 = 风险档「可安全禁用」+ 已启用 + 没被加白。
 *
 *  白名单是用户「这个我要留着」的表态，风险引擎说可禁用也不该勾上它。 */
function recommended(it) {
  return pickable(it) && it.risk === 'safe' && !it.in_whitelist;
}

function findItem(id) {
  return bootState.items.find((x) => x.id === id) || null;
}

/** 从命令行里挖出可执行文件名（不含 .exe，小写）。
 *
 *  与后端 `knowledge::exe_base` 同一套启发式：先找第一个 `.exe`，
 *  再回溯到最后一个**路径分隔符**。分隔符里不能有空格 ——
 *  否则 `C:\A B\my util.exe` 会被截成 `util`，而不是 `my util`。 */
function exeName(cmd) {
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
}

/** 状态徽章。 */
function badgesOf(it) {
  const out = [];
  if (!it.enabled) out.push('<span class="badge off">已禁用</span>');
  if (it.disabled_by_us) out.push('<span class="badge mine">老六禁用</span>');
  if (it.in_whitelist) out.push('<span class="badge white">已加白</span>');
  if (it.in_blacklist) out.push('<span class="badge black">黑名单</span>');
  return out.join('');
}

function renderStartup(scan) {
  const body = $b('#startupBody');
  const list = visibleStartups();
  body.innerHTML = '';

  $b('#startupEmpty').hidden = list.length > 0;

  for (const it of list) {
    const tr = document.createElement('tr');
    tr.className = 'startup-row' + (it.enabled ? '' : ' disabled-row');

    const reason = it.signature_pending ? '正在确认发布者…' : it.risk_reason;
    const impact = it.impact_ms
      ? `<span class="risk-impact">拖慢开机 ${bHumanMs(it.impact_ms)}</span>` : '';
    const state = it.enabled ? (it.delayed ? '已延迟启动' : '已启用') : '已禁用';

    // 「影响」这一列说的是**现在该怎么处理**，不是风险评估报告。
    // 已禁用的条目再显示「可安全禁用」会自相矛盾 —— 用户看到「可以禁用」
    // 却勾不上，只会觉得这软件坏了。这里如实说「已禁用」并告诉他下一步。
    const offNow = !it.enabled;
    const riskTag = offNow
      ? `<span class="risk-tag off" title="风险评估：${bEsc(RISK_LABELS[it.risk] || it.risk)}${it.risk_reason ? '；' + bEsc(it.risk_reason) : ''}">已禁用</span>`
      : `<span class="risk-tag ${bEsc(it.risk)}">${bEsc(RISK_LABELS[it.risk] || it.risk)}</span>`;
    const reasonText = offNow
      ? (it.disabled_by_us ? '由老六禁用，点右侧「启用」可恢复' : '已在系统里被关闭，点右侧「启用」可恢复')
      : reason;
    const cmdTitle = it.command ? ` title="${bEsc(it.command)}"` : '';
    const k = it.knowledge;
    const kbLine = k
      ? `<div class="kb-line" title="${bEsc(k.title)}：${bEsc(k.desc)}">${bEsc(k.title)}${k.vendor ? ' · ' + bEsc(k.vendor) : ''}</div>`
      : `<div class="kb-line none">暂无说明 · 点右侧「说明」补充</div>`;

    // 行内动作。三种意图分得很清楚：
    //   启用   —— 已禁用的条目唯一的出路（否则禁用就是单程票）
    //   延迟   —— 「想留但想更快」，只有自动启动服务有这个能力
    //   说明   —— 知识库 + 加白/加黑/删除的统一入口
    let acts = '';
    if (!it.enabled) {
      acts += `<button class="btn soft sm bx" data-act="enable" data-id="${bEsc(it.id)}">启用</button>`;
    } else if (it.source === 'service' && it.can_disable) {
      acts += it.delayed
        ? `<button class="btn soft sm bx" data-act="undelay" data-id="${bEsc(it.id)}">取消延迟</button>`
        : `<button class="btn soft sm bx" data-act="delay" data-id="${bEsc(it.id)}">延迟</button>`;
    }
    acts += `<button class="btn ghost sm bx" data-info="${bEsc(it.id)}">说明</button>`;

    tr.innerHTML =
      `<td class="c-chk"><label class="chk" title="${it.enabled ? '勾选后可批量禁用' : '已禁用，点右侧「启用」恢复'}"><input type="checkbox" data-id="${bEsc(it.id)}"
         ${bootState.picked.has(it.id) ? 'checked' : ''}
         ${pickable(it) ? '' : 'disabled'}></label></td>` +
      `<td><div class="startup-name">${bEsc(it.name)}${badgesOf(it)}</div>` +
        `<div class="startup-cmd"${cmdTitle}>${bEsc(it.command)}</div>` +
        kbLine + `</td>` +
      `<td><div class="startup-src">${bEsc(SRC_LABELS[it.source] || it.source)}</div>` +
        `<div class="startup-env${it.enabled ? '' : ' off'}">${state}</div></td>` +
      `<td><div class="risk-cell">` +
        riskTag +
        `<span class="risk-reason">${bEsc(reasonText)}</span>` + impact +
      `</div></td>` +
      `<td class="c-act">${acts}</td>`;
    body.appendChild(tr);
  }

  // 勾选状态同步
  body.querySelectorAll('input[data-id]').forEach((cb) => {
    cb.addEventListener('change', () => {
      if (cb.checked) bootState.picked.add(cb.dataset.id);
      else bootState.picked.delete(cb.dataset.id);
      // 只改按钮状态，不重渲染整张表 —— 重渲染会把用户的滚动位置重置掉
      syncDisableBtn();
    });
  });

  // 行内动作。「启用」直接执行（非破坏性、可逆，后端也不为它设风险闸）；
  // 延迟 / 取消延迟要走确认清单，因为它们会改开机行为。
  body.querySelectorAll('button[data-act]').forEach((btn) => {
    const act = btn.dataset.act;
    btn.addEventListener('click', () => {
      if (act === 'enable') enableOne(btn.dataset.id);
      else askAction(act, [btn.dataset.id]);
    });
  });

  // 说明面板
  body.querySelectorAll('button[data-info]').forEach((btn) => {
    btn.addEventListener('click', () => openInfo(btn.dataset.info));
  });

  syncDisableBtn();
  updateStartupSub(scan);

  // 列表重渲染后说明面板里的对象已经换了实例，重新指向新的那一条
  if (bootState.info && !$b('#infoMask').hidden) {
    const fresh = findItem(bootState.info.id);
    if (fresh && fresh !== bootState.info) {
      bootState.info = fresh;
      renderInfo();
    }
  }
}

/** 副标题：先说清「这 N 项里到底有多少能勾」，再说扫描耗时。
 *
 *  用户点「全选」只勾上 30 项时，如果界面上没有任何解释，第一反应就是
 *  「全选坏了」。把「可勾选 / 已禁用 / 不建议禁用」三档摆在标题下，
 *  这个疑问在点之前就消失了。
 */
function updateStartupSub(scan) {
  if (scan) bootState.lastScan = scan;
  paintSubtitle();
}

function paintSubtitle() {
  const el = $b('#startupSub');
  if (!el) return;
  const scan = bootState.lastScan;
  if (!scan) return;

  const total = bootState.items.length;
  const pickN = bootState.items.filter(pickable).length;
  const offN = bootState.items.filter((i) => !i.enabled).length;
  const lockedN = Math.max(0, total - pickN - offN);
  let text = `共 ${total} 项 · 可勾选 ${pickN}` +
    (offN || lockedN ? `（已禁用 ${offN} · 不建议禁用 ${lockedN}）` : '');
  if (bootState.hideDisabled && offN) text += ` · 已隐藏 ${offN} 项已禁用`;

  const pending = scan.pending_signatures || 0;
  if (pending > 0) {
    el.textContent = `${text} · 正在确认 ${pending} 个程序的发布者…`;
    return;
  }
  el.textContent = `${text} · 扫描 ${scan.elapsed_ms} ms` +
    (scan.needs_admin ? ' · 以管理员身份重启可枚举更多项' : '');
}

function syncStartupCounts(counts) {
  const map = {};
  for (const [k, n] of counts) map[k] = n;
  const set = (id, v) => { const el = $b(id); if (el) el.textContent = String(v || 0); };
  set('#cntRunKey', map.run_key);
  set('#cntStartupFolder', map.startup_folder);
  set('#cntService', map.service);
  set('#cntTask', map.scheduled_task);
  const sum = (map.run_key || 0) + (map.startup_folder || 0) + (map.service || 0) + (map.scheduled_task || 0);
  set('#cntStartupAll', sum);
}

/** 勾选项里可能残留已经不在列表里的 ID（重扫之后条目消失了）。
 *  不清掉的话，按钮会显示「禁用选中项（3）」但页面上一个勾都没有。 */
function prunePicked() {
  const alive = new Set(bootState.items.map((i) => i.id));
  for (const id of [...bootState.picked]) {
    if (!alive.has(id)) bootState.picked.delete(id);
  }
}

function syncDisableBtn() {
  prunePicked();
  const n = bootState.picked.size;

  const btn = $b('#btnStartupDisable');
  btn.disabled = n === 0 || !bInvoke;
  btn.textContent = n > 0 ? `禁用选中项（${n}）` : '禁用选中项';

  // 延迟按钮：只有勾选项里存在「可延迟的服务」时才可用。
  // 否则点了只会收到一堆「该来源不支持」，不如直接置灰并说清原因。
  const delayBtn = $b('#btnStartupDelay');
  const delayable = [...bootState.picked].filter((id) => {
    const it = findItem(id);
    return it && it.source === 'service' && it.enabled && it.can_disable;
  }).length;
  delayBtn.disabled = delayable === 0 || !bInvoke;
  delayBtn.textContent = delayable > 0 ? `延迟选中项（${delayable}）` : '延迟选中项';

  // 一键选中推荐项：范围是**当前筛选出的行**，与「全选」同一套语义 ——
  // 用户在「服务」页点它就只选服务，不会偷偷把注册表项也勾上。
  const list = visibleStartups();
  const rec = list.filter(recommended);
  const recLeft = rec.filter((i) => !bootState.picked.has(i.id)).length;
  const recBtn = $b('#btnPickRecommended');
  recBtn.disabled = recLeft === 0 || !bInvoke;
  if (rec.length === 0) recBtn.textContent = '本页无推荐项';
  else if (recLeft === 0) recBtn.textContent = '推荐项已全部选中';
  else recBtn.textContent = `一键选中推荐项（${recLeft}）`;

  const clearBtn = $b('#btnClearPicked');
  clearBtn.disabled = n === 0 || !bInvoke;

  // 「全选」只看当前可见且可勾选的行。勾选项被筛选隐藏时不该让全选框显示
  // 「已全选」—— 那会让用户以为页面上这些都勾上了（这正是之前那个 bug 的形态）。
  const pk = list.filter(pickable);
  const pickedVisible = pk.filter((i) => bootState.picked.has(i.id)).length;
  const all = $b('#chkStartupAll');
  all.disabled = pk.length === 0;
  all.checked = pk.length > 0 && pickedVisible === pk.length;
  all.indeterminate = pickedVisible > 0 && pickedVisible < pk.length;
}

/* ============================================================
   变更流程（禁用 / 延迟启动）：
   先出演练清单 → 用户确认 → 再执行。
   禁用与延迟启动是两种用户意图，按钮分开、确认文案分开，
   但安全链路完全一样：清单必须来自真实演练，不是前端的想象。
   ============================================================ */

/** 每种动作的文案。 */
const ACTION_META = {
  disable: {
    verb: '禁用',
    lead: (n) => `将禁用下面 ${n} 项，全部可一键还原。`,
    blockedLead: (n) => `另有 ${n} 项因安全原因不会改动。`,
    okText: '确认禁用',
    done: (n) => `已禁用 ${n} 项，重启后生效，可随时还原`,
    title: '确认禁用这些启动项？',
  },
  delay: {
    verb: '延迟启动',
    lead: (n) => `将把下面 ${n} 项设为延迟启动：服务保留，但在开机关键路径走完后才拉起。`,
    blockedLead: (n) => `另有 ${n} 项不支持延迟启动（仅自动启动的服务支持）或不会改动。`,
    okText: '确认延迟',
    done: (n) => `已把 ${n} 项设为延迟启动，下次开机生效，可随时还原`,
    title: '确认延迟启动这些启动项？',
  },
  undelay: {
    verb: '取消延迟',
    lead: (n) => `将恢复下面 ${n} 项为普通自动启动。`,
    blockedLead: (n) => `另有 ${n} 项不会改动。`,
    okText: '确认恢复',
    done: (n) => `已恢复 ${n} 项为普通自动启动，下次开机生效，可随时还原`,
    title: '确认恢复这些启动项？',
  },
  enable: {
    verb: '启用',
    lead: (n) => `将把下面 ${n} 项恢复为随开机自启。`,
    blockedLead: (n) => `另有 ${n} 项不会改动。`,
    okText: '确认启用',
    done: (n) => `已启用 ${n} 项，下次开机生效`,
    title: '确认启用这些启动项？',
    hint: '启用只是把启动项恢复成原来的样子，不会安装任何新东西。',
  },
  delete: {
    verb: '删除',
    lead: (n) => `将删除下面 ${n} 项的<b>开机自启</b>设置。`,
    blockedLead: (n) => `另有 ${n} 项因来源或安全原因不能删除。`,
    okText: '确认删除',
    done: (n) => `已删除 ${n} 项自启设置，原值已备份，可在「变更记录」里还原`,
    title: '确认删除这些启动项？',
    hint: '删除前会自动<b>备份原值</b>，随时能在「变更记录」里一键还原。这里删的是「开机自启」，程序本身不会被卸载。',
  },
};

async function askAction(mode, ids) {
  if (!ids || !ids.length) return;
  const meta = ACTION_META[mode];
  if (!meta) { bToast('未知操作：' + mode, true); return; }

  if (!bInvoke) { bToast('预览模式：不会真的修改任何启动项'); return; }

  // 第一步：跑演练，把「到底会动什么」拿到手。
  let plan;
  try {
    plan = await bCall('startup_plan', { ids, action: mode });
  } catch (e) {
    bToast('准备失败：' + e, true);
    return;
  }

  const byId = {};
  for (const it of bootState.items) byId[it.id] = it;

  // 演练里被安全闸拦下的条目，明确告诉用户「这条没动」
  const blocked = plan.changes.filter((c) => !c.ok && c.error);
  const doable = plan.changes.filter((c) => c.ok);

  if (!doable.length) {
    const why = blocked.length
      ? `${blocked.length} 项都无法改动：${blocked[0].error}`
      : '没有可执行的项目';
    bToast(why, true);
    return;
  }

  const list = $b('#confirmList');
  list.innerHTML = '';
  for (const c of doable) {
    const it = byId[c.item_id] || {};
    const li = document.createElement('li');
    li.innerHTML = `<b>${bEsc(it.name || c.name)}</b>` +
      `<span>${bEsc(it.command || '')}</span>`;
    li.title = `${it.name || c.name}\n${it.command || ''}`;
    list.appendChild(li);
  }
  // 拦下来的条目也要列出来并写明原因：只说「另有 N 项不会改动」，
  // 用户会以为是自己勾错了，反复试同一个按钮。
  for (const c of blocked) {
    const it = byId[c.item_id] || {};
    const li = document.createElement('li');
    li.className = 'blocked';
    li.innerHTML = `<b>${bEsc(it.name || c.name)}</b>` +
      `<span class="why">${bEsc(c.error || '不会改动')}</span>`;
    li.title = c.error || '';
    list.appendChild(li);
  }

  let lead = meta.lead(doable.length);
  if (blocked.length) lead += meta.blockedLead(blocked.length);
  // lead / hint 里允许出现 <b>（禁用与删除的语气不同，删除必须把「会备份」讲清楚），
  // 所以走 innerHTML。文案全部来自上面的常量表，没有用户输入，不构成注入面。
  $b('#confirmLead').innerHTML = lead;
  $b('#confirmHint').innerHTML = meta.hint
    || '禁用后这些程序<b>不会</b>再随开机自动运行，你已经安装的程序本身不受影响，随时可以还原。';
  $b('#confirmTitle').textContent = meta.title;
  $b('#confirmOk').textContent = meta.okText;
  $b('#confirmMask').hidden = false;
  $b('#confirmOk')._ids = ids;
  $b('#confirmOk')._action = mode;
}

async function askDisable() {
  askAction('disable', [...bootState.picked]);
}

async function doApply() {
  const ids = $b('#confirmOk')._ids;
  const mode = $b('#confirmOk')._action || 'disable';
  const meta = ACTION_META[mode] || ACTION_META.disable;
  if (!ids || !ids.length) return;
  $b('#confirmOk').disabled = true;
  $b('#confirmOk').textContent = '正在应用…';
  try {
    const res = await bCall('startup_apply', { ids, action: mode });
    const ok = res.changes.filter((c) => c.ok).length;
    const bad = res.changes.filter((c) => !c.ok);
    if (bad.length) {
      const reasons = [...new Set(bad.map((c) => c.error).filter(Boolean))].slice(0, 3).join('；');
      bToast(`已${meta.verb} ${ok} 项；${bad.length} 项未成功（${reasons}）`, true);
    } else {
      bToast(meta.done(ok));
    }
    $b('#confirmMask').hidden = true;
    bootState.picked.clear();
    await loadStartup();
    await loadBatches();
  } catch (e) {
    bToast('应用失败：' + e, true);
  } finally {
    $b('#confirmOk').disabled = false;
    // 复位到静态默认文案：上一次是延迟还是禁用，不该留在按钮上
    $b('#confirmOk').textContent = ACTION_META.disable.okText;
    $b('#confirmOk')._ids = null;
  }
}

/* ============================================================
   还原
   ============================================================ */
async function loadBatches() {
  if (!bInvoke) return;
  try {
    const batches = await bCall('startup_batches');
    const card = $b('#restoreCard');
    const list = $b('#batchList');
    list.innerHTML = '';
    if (!batches || !batches.length) { card.hidden = true; return; }
    card.hidden = false;
    for (const b of batches) {
      // 只列最近的 8 条，避免页面被历史记录撑爆
      const li = document.createElement('li');
      li.innerHTML =
        `<span class="bi">${bEsc(b)}</span>` +
        `<button class="btn soft sm bx" data-batch="${bEsc(b)}">还原</button>`;
      list.appendChild(li);
    }
    list.querySelectorAll('[data-batch]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        if (!bInvoke) return;
        if (!confirm(`还原批次 ${btn.dataset.batch}？\n会把这次禁用的启动项全部恢复原状。`)) return;
        try {
          const r = await bCall('startup_restore', { batchId: btn.dataset.batch });
          bToast(r.ok ? `已还原 ${r.ok} 项` : `还原失败：${r.errors.join('；')}`, !r.ok);
          await loadStartup();
          await loadBatches();
        } catch (e) {
          bToast('还原失败：' + e, true);
        }
      });
    });
  } catch (e) {
    console.warn('[old666] 读取变更批次失败', e);
  }
}

/* ============================================================
   一键启用
   ------------------------------------------------------------
   启用是「把系统恢复原样」：非破坏性、可逆，后端也特意不为它设风险闸。
   再套一层确认弹窗只会让用户多点一次、多犹豫一次，而用户要的正是
   **「保留在列表，可一键启用」**。真实失败原因照样如实回报。
   ============================================================ */
async function enableOne(id) {
  const it = findItem(id);
  if (!it) return;
  if (!bInvoke) { bToast('预览模式：不会真的修改任何启动项'); return; }
  try {
    const res = await bCall('startup_apply', { ids: [id], action: 'enable' });
    const bad = res.changes.filter((c) => !c.ok);
    if (bad.length) bToast(bad[0].error || '启用失败', true);
    else bToast(`已启用「${it.name}」，下次开机生效`);
    await loadStartup();
    await loadBatches();
  } catch (e) {
    bToast('启用失败：' + e, true);
  }
}

/* ============================================================
   启动项说明面板（知识库 + 名单 + 删除）
   ============================================================ */

/** 这条启动项该用哪个名字进名单。
 *
 *  优先用命令行里的 exe 名 —— 那才是「这个东西」的身份，而显示名常带
 *  厂商前缀（`Microsoft OneDrive`），用户脑子里记的是 `onedrive`。
 *  写进名单的串越短越稳：软件升级把显示名改了，短名仍然命中。
 */
function nameOf(it) {
  return exeName(it.command) || it.name || '';
}

function openInfo(id) {
  const it = findItem(id);
  if (!it) return;
  bootState.info = it;
  bootState.editing = false;
  renderInfo();
  $b('#infoMask').hidden = false;
}

function closeInfo() {
  $b('#infoMask').hidden = true;
  bootState.info = null;
  bootState.editing = false;
}

function renderInfo() {
  const it = bootState.info;
  if (!it) return;
  const k = it.knowledge;

  $b('#infoName').textContent = it.name;
  $b('#infoCmd').textContent = it.command || '（没有命令行）';

  // 右上角：这条说明从哪来
  const origin = $b('#infoOrigin');
  origin.textContent = k ? (k.origin === 'user' ? '你的补充' : '内置说明') : '暂无说明';
  origin.className = 'info-origin' + (k && k.origin === 'user' ? ' mine' : '');

  // 标签行：来源 / 状态 / 风险 / 名单
  const tags = [`<span class="tag">${bEsc(SRC_LABELS[it.source] || it.source)}</span>`];
  tags.push(it.enabled
    ? `<span class="tag on">${it.delayed ? '已延迟启动' : '已启用'}</span>`
    : '<span class="tag off">已禁用</span>');
  if (it.disabled_by_us) tags.push('<span class="tag mine">老六禁用</span>');
  tags.push(`<span class="risk-tag ${bEsc(it.risk)}">${bEsc(RISK_LABELS[it.risk] || it.risk)}</span>`);
  if (it.in_whitelist) tags.push('<span class="tag white">白名单</span>');
  if (it.in_blacklist) tags.push('<span class="tag black">黑名单</span>');
  $b('#infoTags').innerHTML = tags.join('');

  // 键值区（dt/dd 与 styles.css 里的两列网格对齐）
  const kv = [];
  if (it.risk_reason) kv.push(`<dt>判定理由</dt><dd>${bEsc(it.risk_reason)}</dd>`);
  if (it.impact_ms) kv.push(`<dt>拖慢开机</dt><dd>${bHumanMs(it.impact_ms)}</dd>`);
  if (it.publisher) kv.push(`<dt>发布者</dt><dd>${bEsc(it.publisher)}</dd>`);
  if (it.location) kv.push(`<dt>位置</dt><dd class="mono">${bEsc(it.location)}</dd>`);
  $b('#infoKv').innerHTML = kv.join('');

  // 说明正文
  const adv = $b('#infoAdvice');
  adv.hidden = false;
  if (k) {
    adv.className = 'info-advice ' + (ADVICE_CLASS[k.advice] || '');
    adv.innerHTML =
      `<div class="adv-title">${bEsc(k.title)}${k.vendor ? ` <em>${bEsc(k.vendor)}</em>` : ''}</div>` +
      `<div class="adv-desc">${bEsc(k.desc)}</div>` +
      (k.advice ? `<div class="adv-advice">建议：<b>${bEsc(k.advice)}</b></div>` : '');
  } else {
    adv.className = 'info-advice unknown';
    adv.innerHTML = '<div class="adv-desc">内置库里没有这个启动项的说明。点「编辑说明」写下它是做什么的 ——' +
      '以后这台机器上同名的启动项都会显示你写的内容，升级程序也不会覆盖它。</div>';
  }

  // 编辑区
  if (bootState.editing) {
    $b('#infoEdit').hidden = false;
    $b('#edKey').value = nameOf(it);
    $b('#edTitle').value = k ? k.title : '';
    $b('#edVendor').value = k ? k.vendor : '';
    $b('#edDesc').value = k ? k.desc : '';
    $b('#edAdvice').value = (k && k.advice) || '谨慎处理';
  } else {
    $b('#infoEdit').hidden = true;
  }

  // 按钮
  $b('#infoEditBtn').hidden = bootState.editing;
  $b('#infoSave').hidden = !bootState.editing;
  $b('#infoReset').hidden = !(k && k.origin === 'user') || bootState.editing;
  const nm = nameOf(it);
  $b('#infoWhite').disabled = !nm;
  $b('#infoBlack').disabled = !nm;
  $b('#infoWhite').textContent = it.in_whitelist ? '移出白名单' : '加入白名单';
  $b('#infoBlack').textContent = it.in_blacklist ? '移出黑名单' : '加入黑名单并禁用';

  // 删除：只有注册表值与启动文件夹的快捷方式能删 ——
  // 服务 / 计划任务是系统基础设施，删掉「自启」等于动系统组件，
  // 后端也拒绝（见 startup_ctl.rs 文件头），这里如实置灰并说明原因。
  const deletable = it.source === 'run_key' || it.source === 'startup_folder';
  $b('#infoDelete').disabled = !deletable;
  $b('#infoDelete').title = deletable
    ? '删除这条开机自启（会先备份原值）'
    : '服务与计划任务不支持删除自启，请改用「禁用」';

  $b('#infoHint').innerHTML = deletable
    ? '「删除」会移除这条开机自启并<b>备份原值</b>（可在「变更记录」里还原）；「禁用」则保留它、随时可启用。'
    : '这个来源不支持删除自启（删掉会伤系统），请改用「禁用」—— 同样可随时启用回来。';
}

/** 保存一条说明到用户库。 */
async function saveKnowledge() {
  const it = bootState.info;
  if (!it) return;
  const key = $b('#edKey').value.trim();
  const title = $b('#edTitle').value.trim();
  const vendor = $b('#edVendor').value.trim();
  const desc = $b('#edDesc').value.trim();
  const advice = $b('#edAdvice').value;
  if (!key) { bToast('匹配串不能为空（它决定这条说明对哪些启动项生效）', true); return; }
  if (!title) { bToast('标题不能为空', true); return; }
  if (!desc) { bToast('请写下它是做什么的', true); return; }
  try {
    await bCall('knowledge_save', { key, title, vendor, desc, advice });
    bootState.editing = false;
    bToast('说明已保存');
    await loadStartup();
    await loadKnowledge();
  } catch (e) {
    bToast('保存失败：' + e, true);
  }
}

/** 恢复内置说明：删掉用户库里命中的那一条。 */
async function resetKnowledge() {
  const it = bootState.info;
  const k = it && it.knowledge;
  if (!k || k.origin !== 'user') return;
  try {
    await bCall('knowledge_remove', { key: k.key });
    bToast('已恢复内置说明');
    await loadStartup();
    await loadKnowledge();
  } catch (e) {
    bToast('恢复失败：' + e, true);
  }
}

/**
 * 加 / 移名单。
 *
 * **加入黑名单会立刻禁用**（后端在一次调用里做完），因为用户点这一下的
 * 时候就已经表态了：「加入黑名单的不需要过问直接禁用」。
 * 移出黑名单**不会**自动启用回来 —— 那是另一个决定，由用户点「启用」发出。
 */
async function toggleList(kind) {
  const it = bootState.info;
  if (!it) return;
  const nm = nameOf(it);
  if (!nm) { bToast('这条启动项没有可用的名称', true); return; }
  const inList = kind === 'white' ? it.in_whitelist : it.in_blacklist;
  const label = kind === 'white' ? '白名单' : '黑名单';
  try {
    if (inList) {
      await bCall('startup_list_remove', { kind, name: nm });
      bToast(`已把「${nm}」移出${label}`);
    } else {
      const out = await bCall('startup_list_add', { kind, name: nm });
      if (kind === 'black') {
        if (out.dry_run) bToast('演练模式：已加入黑名单，但没有真的禁用');
        else if (out.disabled > 0) bToast(`已加入黑名单，并直接禁用 ${out.disabled} 项`);
        else if (out.failed && out.failed.length) bToast(`已加入黑名单，但禁用失败：${out.failed[0]}`, true);
        else bToast('已加入黑名单（这条当前不需要禁用）');
      } else {
        bToast(`已把「${nm}」加入白名单，不会出现在「推荐选中」里`);
      }
    }
    await loadStartup();
    await loadSettingsExtras();
  } catch (e) {
    bToast(`${label}操作失败：` + e, true);
  }
}

/** 删除启动项（先备份，再删）。 */
async function askDelete() {
  const it = bootState.info;
  if (!it) return;
  if (!bInvoke) { bToast('预览模式：不会真的修改任何启动项'); return; }
  closeInfo();
  await askAction('delete', [it.id]);
}

/* ============================================================
   设置抽屉里的两个外置 JSON：知识库 + 名单
   ============================================================ */
async function loadKnowledge() {
  if (!bInvoke) return;
  try {
    const kb = await bCall('knowledge_stats');
    $b('#kbBuiltin').textContent = String(kb.builtin);
    $b('#kbUser').textContent = String(kb.user);
    $b('#kbPath').textContent = kb.user_path;
    const ul = $b('#kbList');
    ul.innerHTML = '';
    const entries = kb.user_entries || [];
    if (!entries.length) {
      ul.innerHTML = '<li class="empty">还没有你的补充。在列表里点某个启动项的「说明」→「编辑说明」就能加。</li>';
      return;
    }
    for (const e of entries) {
      const li = document.createElement('li');
      li.innerHTML =
        `<span class="k" title="${bEsc(e.key)}">${bEsc(e.key)}</span>` +
        `<span class="n">${bEsc(e.title)}</span>` +
        `<button class="btn ghost sm" data-kbrm="${bEsc(e.key)}">删掉</button>`;
      ul.appendChild(li);
    }
  } catch (e) {
    console.warn('[old666] 读取知识库失败', e);
  }
}

async function loadStartupLists() {
  if (!bInvoke) return;
  try {
    const l = await bCall('startup_lists');
    const ul = $b('#listRules');
    $b('#listPath').textContent = l.path;
    ul.innerHTML = '';
    const rows = [
      ...(l.whitelist || []).map((n) => ({ n, kind: 'white', label: '白名单' })),
      ...(l.blacklist || []).map((n) => ({ n, kind: 'black', label: '黑名单' })),
    ];
    if (!rows.length) {
      ul.innerHTML = '<li class="empty">名单是空的。在启动项的「说明」里可以加入白名单或黑名单。</li>';
      return;
    }
    for (const r of rows) {
      const li = document.createElement('li');
      li.innerHTML =
        `<span class="tag ${r.kind}">${r.label}</span>` +
        `<span class="n mono">${bEsc(r.n)}</span>` +
        `<button class="btn ghost sm" data-listrm="${bEsc(r.kind)}:${bEsc(r.n)}">移出</button>`;
      ul.appendChild(li);
    }
  } catch (e) {
    console.warn('[old666] 读取启动项名单失败', e);
  }
}

/** 设置抽屉打开时调用（由 main.js 触发）。 */
async function loadSettingsExtras() {
  await Promise.all([loadKnowledge(), loadStartupLists()]);
}

/* ============================================================
   事件绑定
   ============================================================ */
function bindBoot() {
  const first = $b('#btnStartupDisable');
  if (!first || first._bootBound) return;
  first._bootBound = true;

  $b('#btnStartupDisable').addEventListener('click', askDisable);
  $b('#btnStartupDelay').addEventListener('click', () => askAction('delay', [...bootState.picked]));
  $b('#confirmOk').addEventListener('click', doApply);
  $b('#confirmCancel').addEventListener('click', () => { $b('#confirmMask').hidden = true; });
  $b('#confirmMask').addEventListener('click', (e) => {
    if (e.target === $b('#confirmMask')) $b('#confirmMask').hidden = true;
  });

  $b('#startupFilters').addEventListener('click', (e) => {
    const b = e.target.closest('[data-src]');
    if (!b) return;
    bootState.srcFilter = b.dataset.src;
    $b('#startupFilters').querySelectorAll('.chip').forEach((x) => x.classList.remove('active'));
    b.classList.add('active');
    renderStartup(null);
  });

  let sTimer = null;
  $b('#startupSearch').addEventListener('input', (e) => {
    bootState.search = e.target.value;
    clearTimeout(sTimer);
    sTimer = setTimeout(() => renderStartup(null), 120);
  });

  // 全选 / 取消全选。
  //
  // **只作用于「当前筛选出来的、且真的可勾选的」行** —— 与 `syncDisableBtn`
  // 里的判定必须是同一套 `pickable()`，否则全选框会显示「已全选」而页面上
  // 还有没勾上的行（用户报的「点了没反应/勾不上」就是这个形态）。
  $b('#chkStartupAll').addEventListener('change', (e) => {
    const all = visibleStartups();
    const list = all.filter(pickable);
    for (const it of list) {
      if (e.target.checked) bootState.picked.add(it.id);
      else bootState.picked.delete(it.id);
    }
    renderStartup(null);
    // 「全选」只能勾得动可勾的行。剩下的当场解释，否则用户只会觉得「全选坏了」。
    if (e.target.checked) {
      const skipped = all.length - list.length;
      if (skipped > 0) {
        const offN = all.filter((i) => !i.enabled).length;
        bToast(`已选中 ${list.length} 项；另有 ${skipped} 项不可勾选（已禁用 ${offN}、不建议禁用 ${skipped - offN}）`);
      }
    }
  });

  // 「隐藏已禁用」：默认关（用户需要能看到全部家底），勾上后只留下待处理的。
  const hideCb = $b('#chkHideDisabled');
  if (hideCb) {
    hideCb.checked = bootState.hideDisabled;
    hideCb.addEventListener('change', () => {
      bootState.hideDisabled = hideCb.checked;
      saveHidePref(hideCb.checked);
      renderStartup(null);
      // 之前被藏起来的条目可能还没补签，重新露出来时顺手补上，
      // 否则它们会一直停在「正在确认发布者…」。
      if (visibleStartups().some((i) => i.signature_pending)) verifyStartup();
    });
  }

  // 一键选中推荐项：同样以「当前筛选出的行」为范围。
  $b('#btnPickRecommended').addEventListener('click', () => {
    for (const it of visibleStartups().filter(recommended)) bootState.picked.add(it.id);
    renderStartup(null);
    const n = [...bootState.picked].length;
    if (n > 0) bToast(`已选中 ${n} 项推荐禁用的启动项，确认后一起处理`);
  });

  $b('#btnClearPicked').addEventListener('click', () => {
    bootState.picked.clear();
    renderStartup(null);
  });

  bindInfo();
  bindSettingsExtras();

  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    if (!$b('#confirmMask').hidden) $b('#confirmMask').hidden = true;
    else if (!$b('#infoMask').hidden) closeInfo();
  });
}

/** 说明面板的事件绑定。 */
function bindInfo() {
  const mask = $b('#infoMask');
  if (!mask || mask._bound) return;
  mask._bound = true;

  mask.addEventListener('click', (e) => { if (e.target === mask) closeInfo(); });
  $b('#infoX').addEventListener('click', closeInfo);
  $b('#infoClose').addEventListener('click', closeInfo);

  $b('#infoEditBtn').addEventListener('click', () => {
    bootState.editing = true;
    renderInfo();
    $b('#edTitle').focus();
  });
  $b('#infoSave').addEventListener('click', saveKnowledge);
  $b('#infoReset').addEventListener('click', resetKnowledge);
  $b('#infoWhite').addEventListener('click', () => toggleList('white'));
  $b('#infoBlack').addEventListener('click', () => toggleList('black'));
  $b('#infoDelete').addEventListener('click', askDelete);
}

/** 设置抽屉里「知识库 / 名单」两块的事件绑定。 */
function bindSettingsExtras() {
  const kbList = $b('#kbList');
  if (kbList && !kbList._bound) {
    kbList._bound = true;
    kbList.addEventListener('click', async (e) => {
      const b = e.target.closest('[data-kbrm]');
      if (!b || !bInvoke) return;
      try {
        await bCall('knowledge_remove', { key: b.dataset.kbrm });
        bToast('已删掉这条补充说明');
        await loadKnowledge();
        await loadStartup();
      } catch (err) {
        bToast('删除失败：' + err, true);
      }
    });
  }
  const rules = $b('#listRules');
  if (rules && !rules._bound) {
    rules._bound = true;
    rules.addEventListener('click', async (e) => {
      const b = e.target.closest('[data-listrm]');
      if (!b || !bInvoke) return;
      const i = b.dataset.listrm.indexOf(':');
      const kind = b.dataset.listrm.slice(0, i);
      const name = b.dataset.listrm.slice(i + 1);
      try {
        await bCall('startup_list_remove', { kind, name });
        bToast('已移出名单');
        await loadStartupLists();
        await loadStartup();
      } catch (err) {
        bToast('移出失败：' + err, true);
      }
    });
  }
}

/** 视图切到开机加速时调用（由 main.js 的导航触发）。 */
async function enterBootView() {
  bindBoot();
  if (!bootState.timeline || !bootState.items.length) {
    await Promise.all([loadBoot(), loadStartup(), loadBatches()]);
  } else {
    await loadBoot();
  }
}

bindBoot();
