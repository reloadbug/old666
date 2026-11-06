/* ============================================================
   老六清净 old666 · 场景模式视图（P3）
   ------------------------------------------------------------
   设计原则（与后端一一对应）：
   1. **先看计划，再动手** —— 点「应用」先调 `mode_plan` 拿到编排结果，
      把「会做什么、不会做什么、为什么」逐条摆出来，用户点头后才调
      `mode_apply`。绝不「点了就改」。
   2. **结束进程必须逐次确认** —— 计划里存在会终止进程的动作时，
      弹窗里必须逐条列出受影响的对象；用户不点头就一个进程都不结束。
      后端 `mode_apply` 的 `kill_confirmed` 闸是最后一道防线，
      前端不敢越过后端。
   3. **浏览器预览不造假** —— 没有 Tauri 时诚实地说明本页需要真实环境，
      绝不渲染一份编造的模式列表。
   ============================================================ */

/* 这里刻意不访问 main.js 里的 `$` / `invoke`：
   启动顺序不固定，自己持一份引用最稳。 */
const $m = (s) => document.querySelector(s);
const mInvoke = (typeof window !== 'undefined' && window.__TAURI__ && window.__TAURI__.core)
  ? window.__TAURI__.core.invoke
  : null;

/* ---------------------- 本地状态 ---------------------- */
const modeState = {
  /** 后端返回的全部模式（内置 + 自定义）。 */
  modes: [],
  /** 当前生效模式 id。 */
  active: 'daily',
  /** 自动切换配置。 */
  auto: { enabled: false },
  /** 当前选中（正在编排）的模式 id。 */
  selected: null,
  /** 最近一次编排结果。 */
  plan: null,
  /** 电源计划 GUID → 中文名。 */
  powerName: '',
  /** 内存占用百分比。 */
  memPct: 0,
  /** 免打扰是否开启。 */
  focusAssist: false,
};

/* ---------------------- 工具 ---------------------- */

function mEsc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

/** 轻量提示。刻意不复用 main.js 的 toast —— 脚本加载顺序不固定。 */
function mToast(msg, isErr) {
  const fn = (typeof window !== 'undefined' && typeof toast === 'function') ? toast : null;
  if (fn) { fn(msg, isErr); return; }
  const el = document.getElementById('toast');
  if (!el) { alert(msg); return; }
  el.textContent = msg;
  el.className = 'toast' + (isErr ? ' err' : '');
  el.hidden = false;
  clearTimeout(el._mt);
  el._mt = setTimeout(() => { el.hidden = true; }, 2800);
}

/** 统一解包后端 `Reply<T>`（`{ok, data, error}`）。 */
async function mCall(cmd, args) {
  if (!mInvoke) throw new Error('no-tauri');
  const r = await mInvoke(cmd, args);
  if (r && typeof r === 'object' && typeof r.ok === 'boolean' && 'data' in r) {
    if (!r.ok) throw new Error(r.error || '操作失败');
    return r.data;
  }
  return r;
}

/** 动作名 → 中文标签。 */
const ACTION_LABELS = {
  purge_processes: '结束闲置进程',
  trim_memory: '裁剪工作集',
  set_power_plan: '切换电源计划',
  boost_foreground_priority: '提升前台优先级',
  set_focus_assist: '系统通知免打扰',
  delay_startup: '延迟启动项',
  protect_critical_services: '关键服务保护',
  require_double_confirm: '变更二次确认',
};

/** 电源计划 GUID → 中文名。 */
const POWER_PLANS = {
  '381b4222-f694-41f0-9685-ff5bb260df2e': '平衡',
  '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c': '高性能',
  'e9a42b02-d5df-448d-aa00-03f14749eb61': '节能',
  '9a42b02-d5df-448d-aa00-03f14749eb61': '卓越性能',
};

/* ============================================================
   加载与渲染
   ============================================================ */
async function loadModes() {
  if (!mInvoke) {
    $m('#modeStateSub').textContent = '预览模式：此页需要真实系统环境';
    $m('#modeActiveName').textContent = '预览模式';
    $m('#modeMemPct').textContent = '—';
    $m('#modeGrid').innerHTML =
      '<div class="empty">当前在浏览器里预览，读取不到真实的场景模式。' +
      '请运行「cargo run --release --bin old666」查看本页效果。</div>';
    return;
  }
  let d;
  try {
    d = await mCall('mode_list');
  } catch (e) {
    mToast('读取场景模式失败：' + e, true);
    return;
  }
  modeState.modes = d.modes || [];
  modeState.active = d.active || 'daily';
  modeState.auto = d.auto_switch || { enabled: false };
  modeState.powerName = POWER_PLANS[(d.power_plan || '').toLowerCase()] || (d.power_plan || '');
  modeState.memPct = d.mem_usage_pct || 0;
  modeState.focusAssist = !!d.focus_assist;

  renderState();
  renderGrid();
  renderAutoForm();
  checkAutoSwitch();
}

function renderState() {
  const cur = modeState.modes.find((m) => m.id === modeState.active);
  $m('#modeActiveName').textContent = cur ? cur.name : modeState.active;
  $m('#modeMemPct').textContent = `内存占用 ${modeState.memPct}%`;
  const bits = [];
  if (modeState.powerName) bits.push(`电源：${modeState.powerName}`);
  bits.push(modeState.focusAssist ? '免打扰：开' : '免打扰：关');
  bits.push(modeState.auto && modeState.auto.enabled ? '自动切换：开' : '自动切换：关');
  $m('#modeStateSub').textContent = bits.join(' · ');
}

/* ---------------------- 自动切换判定 ---------------------- */

/**
 * 问后端「现在该不该切模式」。
 *
 * 注意这**只提示，不自作主张**：
 * 后端 `mode_auto_check` 零副作用，切不切由用户点「切过去」决定。
 * 自动切换如果顺手把动作做了，那条红线就被绕过去了。
 */
async function checkAutoSwitch() {
  if (!mInvoke) return;
  let d;
  try {
    d = await mCall('mode_auto_check');
  } catch (e) {
    return;
  }
  const notice = $m('#modeAutoNotice');
  if (!d || !d.should_switch || !d.target_mode) {
    notice.hidden = true;
    return;
  }
  const target = modeState.modes.find((m) => m.id === d.target_mode);
  modeState.pendingAutoTarget = d.target_mode;
  $m('#modeAutoNoticeText').innerHTML =
    `检测到你像是进入了「<b>${mEsc(target ? target.name : d.target_mode)}</b>」场景。` +
    '<span class="mode-notice-why">自动切换只是提醒，切换后仍需你点「按计划执行」才会真的生效。</span>';
  notice.hidden = false;
}

function renderGrid() {
  const grid = $m('#modeGrid');
  grid.innerHTML = '';
  for (const m of modeState.modes) {
    const card = document.createElement('div');
    const active = m.id === modeState.active;
    const selected = m.id === modeState.selected;
    card.className = 'mode-card' + (active ? ' active' : '') + (selected ? ' selected' : '');
    const rules = (m.rules || []).length;
    card.dataset.mode = m.id;

    const tag = m.builtin
      ? '<span class="mode-badge builtin">内置</span>'
      : '<span class="mode-badge custom">自定义</span>';
    // 「会不会结束进程」从规则本身推出来 —— 前端不依赖后端额外字段，
    // 免得两边对不上。
    const kills = (m.rules || []).some((r) => r.then && r.then.action === 'purge_processes');
    const acts = kills
      ? '<span class="mode-badge danger">会结束进程</span>'
      : '';

    card.innerHTML =
      `<div class="mode-card-head">${tag}${acts}</div>` +
      `<div class="mode-card-title">${mEsc(m.name)}</div>` +
      `<div class="mode-card-tagline">${mEsc(m.tagline || m.desc || '')}</div>` +
      `<div class="mode-card-meta">${rules} 条规则` +
      (m.settings && m.settings.mem_threshold_pct
        ? ` · 内存阈值 ${m.settings.mem_threshold_pct}%` : '') +
      '</div>';
    grid.appendChild(card);
  }
}

function renderAutoForm() {
  const a = modeState.auto || {};
  $m('#modeAutoEnabled').value = a.enabled ? '1' : '0';
  // 注意：**不要**在这里动 #modeAutoCard 的 hidden —— 那是按钮 toggle 的状态，
  // 每次刷新都把它掀开/盖上会让按钮的行为变得不可预测。

  const opts = modeState.modes
    .map((m) => `<option value="${mEsc(m.id)}">${mEsc(m.name)}</option>`)
    .join('');
  const memSel = $m('#modeAutoMemMode');
  const fgSel = $m('#modeAutoFgMode');
  const oldMem = memSel.value;
  const oldFg = fgSel.value;
  memSel.innerHTML = '<option value="">（不设置）</option>' + opts;
  fgSel.innerHTML = '<option value="">（不设置）</option>' + opts;
  if (a.mem_above_mode) memSel.value = a.mem_above_mode;
  else if (oldMem) memSel.value = oldMem;
  if (a.foreground_mode) fgSel.value = a.foreground_mode;
  else if (oldFg) fgSel.value = oldFg;
  $m('#modeAutoFgProc').value = a.foreground_process || '';
  $m('#btnModeAuto').textContent = '自动切换：' + (a.enabled ? '开' : '关');
}

/* ============================================================
   编排与计划
   ============================================================ */
async function planMode(modeId) {
  modeState.selected = modeId;
  renderGrid();
  if (!mInvoke) { mToast('预览模式：不会真的执行任何操作'); return; }

  let plan;
  try {
    plan = await mCall('mode_plan', { modeId });
  } catch (e) {
    mToast('编排失败：' + e, true);
    return;
  }
  modeState.plan = plan;
  renderPlan(plan);
}

/** 把计划渲染成逐条可读说明。 */
function renderPlan(plan) {
  const card = $m('#modePlanCard');
  card.hidden = false;
  $m('#modePlanTitle').textContent = `计划：${plan.mode_name}`;
  const sub = [];
  sub.push(`当前内存 ${plan.mem_usage_pct}%`);
  if (plan.terminates_process) sub.push('含会结束进程的动作，需要你逐次确认');
  if (plan.require_double_confirm) sub.push('本模式要求二次确认');
  if (plan.excludes && plan.excludes.length) sub.push(`与 ${plan.excludes.join('、')} 互斥`);
  $m('#modePlanSub').textContent = sub.join(' · ');

  const list = $m('#modeRuleList');
  list.innerHTML = '';
  for (const r of plan.rules || []) {
    const li = document.createElement('li');
    li.className = 'mode-rule' + (r.triggered ? '' : ' off');

    const label = ACTION_LABELS[r.action] || r.action;
    const head =
      `<div class="mode-rule-head">` +
      `<b>${mEsc(r.title)}</b>` +
      `<span class="mode-rule-action">${mEsc(label)}</span>` +
      (r.triggered ? '' : '<span class="mode-rule-skip">条件未满足</span>') +
      (r.needs_confirm ? '<span class="mode-rule-warn">需确认</span>' : '') +
      `</div>`;

    const why = `<div class="mode-rule-why">${mEsc(r.when)}</div>`;
    const explain = r.explain ? `<div class="mode-rule-explain">${mEsc(r.explain)}</div>` : '';

    let targets = '';
    if (r.targets && r.targets.length) {
      const items = r.targets.slice(0, 12).map((t) => `<li>${mEsc(t)}</li>`).join('');
      const more = r.targets.length > 12
        ? `<li class="more">…还有 ${r.targets.length - 12} 项</li>` : '';
      targets =
        '<div class="mode-rule-targets"><div class="mode-rule-targets-title">将影响：</div>' +
        `<ul>${items}${more}</ul></div>`;
    }

    li.innerHTML = head + why + explain + targets;
    list.appendChild(li);
  }
}

/* ============================================================
   执行（红线：结束进程必须逐次确认）
   ============================================================ */
async function applyMode() {
  const plan = modeState.plan;
  if (!plan) { mToast('还没有计划，请先选择模式'); return; }
  if (!mInvoke) { mToast('预览模式：不会真的执行任何操作'); return; }

  // 有会终止进程的动作时，必须弹确认框列出受影响对象。
  const killing = (plan.rules || []).filter((r) =>
    r.triggered && r.action === 'purge_processes');
  let killConfirmed = false;

  if (killing.length) {
    const ok = await confirmKill(plan, killing);
    if (!ok) return;
    killConfirmed = true;
  }

  const btn = $m('#btnModeApply');
  btn.disabled = true;
  btn.textContent = '正在执行…';
  try {
    const r = await mCall('mode_apply', {
      modeId: plan.mode_id,
      ruleIds: [],
      killConfirmed,
      graceMs: 800,
    });
    renderApplyResult(r);
    await loadModes();
    await planMode(plan.mode_id);
  } catch (e) {
    mToast('执行失败：' + e, true);
  } finally {
    btn.disabled = false;
    btn.textContent = '按计划执行';
  }
}

/** 结束进程前的确认框。返回 true = 用户点头。 */
function confirmKill(plan, killingRules) {
  return new Promise((resolve) => {
    const mask = $m('#confirmMask');
    const list = $m('#confirmList');
    list.innerHTML = '';

    const lead =
      `模式「${plan.mode_name}」里有 <b>会结束进程</b>的动作。` +
      `下面这些是将被结束的对象（只包含系统判定为完全闲置、没有窗口、不是前台关联的程序）：`;
    $m('#confirmLead').innerHTML = lead;

    for (const r of killingRules) {
      const li = document.createElement('li');
      li.innerHTML = `<b>${mEsc(r.title)}</b>` +
        `<span>${mEsc(r.when)}</span>`;
      list.appendChild(li);
    }
    const li = document.createElement('li');
    li.className = 'blocked';
    li.innerHTML = '<b>结束哪些进程</b>' +
      '<span class="why">执行时会重新扫描并逐个复核；任何有窗口、在前台、有磁盘读写的程序都不会被结束</span>';
    list.appendChild(li);

    $m('#confirmHint').innerHTML =
      '不点头就<b>一个进程都不会结束</b>，其余不涉及结束进程的动作（裁剪内存、切换电源计划等）仍会照常执行。';
    $m('#confirmTitle').textContent = '确认执行会结束进程的动作？';
    $m('#confirmOk').textContent = '确认执行';

    const ok = $m('#confirmOk');
    const cancel = $m('#confirmCancel');
    const onOk = () => { cleanup(); resolve(true); };
    const onCancel = () => { cleanup(); resolve(false); };
    function cleanup() {
      mask.hidden = true;
      ok.removeEventListener('click', onOk);
      cancel.removeEventListener('click', onCancel);
    }
    ok.addEventListener('click', onOk);
    cancel.addEventListener('click', onCancel);
    mask.hidden = false;
  });
}

/** 执行结果逐条展示。 */
function renderApplyResult(r) {
  const card = $m('#modePlanCard');
  card.hidden = false;
  $m('#modePlanTitle').textContent = `执行结果：${r.mode_name}`;
  $m('#modePlanSub').textContent = r.summary || '';

  const list = $m('#modeRuleList');
  list.innerHTML = '';
  for (const a of r.actions || []) {
    const li = document.createElement('li');
    li.className = 'mode-rule' + (a.ok ? '' : ' off');
    li.innerHTML =
      `<div class="mode-rule-head">` +
      `<b>${mEsc(a.rule_id || a.action)}</b>` +
      `<span class="mode-rule-action">${mEsc(ACTION_LABELS[a.action] || a.action)}</span>` +
      (a.skipped ? '<span class="mode-rule-skip">已跳过</span>' : '') +
      `</div>` +
      `<div class="mode-rule-explain">${mEsc(a.detail)}</div>`;
    list.appendChild(li);
  }
  mToast(r.summary || '已执行');
}

/* ============================================================
   自动切换
   ============================================================ */
async function saveAuto() {
  if (!mInvoke) { mToast('预览模式：不会真的执行任何操作'); return; }
  const enabled = $m('#modeAutoEnabled').value === '1';
  const memMode = $m('#modeAutoMemMode').value || null;
  const fgMode = $m('#modeAutoFgMode').value || null;
  const fgProc = $m('#modeAutoFgProc').value.trim() || null;
  const pct = modeState.modes.find((m) => m.id === memMode);
  try {
    await mCall('mode_auto_set', {
      enabled,
      memAbovePct: pct ? (pct.settings && pct.settings.mem_threshold_pct) || 85 : null,
      memAboveMode: memMode,
      foregroundProcess: fgProc,
      foregroundMode: fgMode,
    });
    mToast('自动切换设置已保存');
    await loadModes();
  } catch (e) {
    mToast('保存失败：' + e, true);
  }
}

/* ============================================================
   自定义模式
   ============================================================ */

const MODE_TEMPLATE = {
  id: 'my_mode',
  name: '我的模式',
  tagline: '一句话说明这个模式做什么',
  desc: '',
  icon: '',
  builtin: false,
  mutually_exclusive_with: [],
  settings: { mem_threshold_pct: 80, power_plan: 'balanced' },
  rules: [
    {
      id: 'my.trim',
      title: '裁剪闲置进程的工作集',
      desc: '',
      priority: 10,
      when: { kind: 'mem_usage_above', pct: 80 },
      then: { action: 'trim_memory', min_working_set_mb: 64 },
      explain: '只裁剪内存，不结束任何进程。',
    },
  ],
};

function openModeEditor() {
  const existing = modeState.modes.find((m) => !m.builtin && m.id === modeState.selected);
  const src = existing
    ? JSON.stringify({ ...existing, builtin: false, source_file: undefined }, null, 2)
    : JSON.stringify(MODE_TEMPLATE, null, 2);
  $m('#modeJson').value = src;
  $m('#modeModalHint').innerHTML =
    '必须包含 <code>id</code> / <code>name</code> / <code>rules</code>；' +
    '每条规则需要 <code>id</code> / <code>title</code> / <code>when</code> / <code>then</code>。';
  $m('#modeMask').hidden = false;
}

async function saveMode() {
  if (!mInvoke) { mToast('预览模式：不会真的执行任何操作'); return; }
  const json = $m('#modeJson').value;
  try {
    const id = await mCall('mode_custom_save', { json });
    $m('#modeMask').hidden = true;
    mToast('自定义模式「' + id + '」已保存');
    await loadModes();
    await planMode(id);
  } catch (e) {
    mToast('保存失败：' + e, true);
  }
}

/* ============================================================
   入口
   ============================================================ */
function enterModeView() {
  loadModes();
}

function bindMode() {
  $m('#modeGrid').addEventListener('click', (e) => {
    const card = e.target.closest('[data-mode]');
    if (!card) return;
    planMode(card.dataset.mode);
  });
  $m('#btnModeApply').addEventListener('click', applyMode);
  $m('#btnModePlanRefresh').addEventListener('click', () => {
    if (modeState.selected) planMode(modeState.selected);
  });
  $m('#btnModeAuto').addEventListener('click', () => {
    const card = $m('#modeAutoCard');
    card.hidden = !card.hidden;
  });
  $m('#btnModeAutoSave').addEventListener('click', saveAuto);
  $m('#btnModeSwitch').addEventListener('click', async () => {
    const notice = $m('#modeAutoNotice');
    const target = modeState.pendingAutoTarget;
    if (!target) { notice.hidden = true; return; }
    try {
      await mCall('mode_set_active', { modeId: target });
      mToast('已切换到「' + target + '」');
      notice.hidden = true;
      await loadModes();
      await planMode(target);
    } catch (e) {
      mToast('切换失败：' + e, true);
    }
  });
  $m('#btnModeSwitchDismiss').addEventListener('click', () => {
    $m('#modeAutoNotice').hidden = true;
  });
  $m('#btnModeCustom').addEventListener('click', openModeEditor);
  $m('#modeModalSave').addEventListener('click', saveMode);
  $m('#modeModalClose').addEventListener('click', () => { $m('#modeMask').hidden = true; });
  $m('#modeModalX').addEventListener('click', () => { $m('#modeMask').hidden = true; });
  $m('#modeMask').addEventListener('click', (e) => {
    if (e.target === $m('#modeMask')) $m('#modeMask').hidden = true;
  });
}

if (typeof document !== 'undefined') {
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bindMode);
  } else {
    bindMode();
  }
}
