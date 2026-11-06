/* ============================================================
   老六清净 old666 · 前端逻辑
   ------------------------------------------------------------
   两种运行模式：
   1. Tauri 内（window.__TAURI__ 存在）—— 走真实系统调用；
   2. 纯浏览器（无 Tauri）—— 走内置 mock 数据，方便设计与预览，
      且**绝不**假装做了真实操作（所有按钮会提示"预览模式"）。
   ============================================================ */

const IS_TAURI = typeof window.__TAURI__ !== 'undefined' && !!window.__TAURI__.core;
const invoke = IS_TAURI ? window.__TAURI__.core.invoke : null;

/* ---------------------- 状态 ---------------------- */
const state = {
  list: [],
  metrics: null,
  stats: null,
  filter: 'all',
  search: '',
  selected: new Set(),
  timer: null,
  interval: 2000,        // 自适应刷新间隔（1–5s）
  fullScanCounter: 0,
  busy: false,
  whitelist: { names: [], paths: [] },
  snapshots: [],
};

/* ---------------------- 工具 ---------------------- */
const $ = (s) => document.querySelector(s);

function humanBytes(b) {
  if (!b) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return (i === 0 ? v.toFixed(0) : v.toFixed(1)) + ' ' + u[i];
}

function humanRate(bps) {
  return humanBytes(Math.max(0, bps || 0)) + '/s';
}

function humanUptime(sec) {
  if (!sec) return '—';
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (d) return `${d}天${h}时`;
  if (h) return `${h}时${m}分`;
  return `${m}分`;
}

function esc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function toast(msg, isErr) {
  const el = $('#toast');
  el.textContent = msg;
  el.className = 'toast' + (isErr ? ' err' : '');
  el.hidden = false;
  clearTimeout(el._t);
  el._t = setTimeout(() => { el.hidden = true; }, 2600);
}

/* ============================================================
   Mock 数据（仅浏览器预览用）
   ============================================================ */
const MOCK = [
  { name: 'chrome.exe', pid: 8124, cpu: 3.2, ws: 512 * 1048576, sig: 'valid', sigName: 'Google LLC', verdict: 'forbidden', group: '浏览器', reasons: ['当前正在前台使用'] },
  { name: 'Code.exe', pid: 4412, cpu: 1.1, ws: 380 * 1048576, sig: 'valid', sigName: 'Microsoft Corporation', verdict: 'forbidden', group: '其他应用', reasons: ['有可见窗口，可能正在用'] },
  { name: 'WeChat.exe', pid: 9931, cpu: 0.4, ws: 264 * 1048576, sig: 'valid', sigName: 'Tencent Technology', verdict: 'keep', group: '即时通讯', reasons: ['有可见窗口，可能正在用'] },
  { name: 'update-notifier.exe', pid: 15022, cpu: 0.0, ws: 78 * 1048576, sig: 'unsigned', sigName: null, verdict: 'safe', group: '后台服务', reasons: ['无窗口、无前台关联', 'CPU 占用 0.0%，几乎无活动', '无数字签名'] },
  { name: 'AdobeIPCBroker.exe', pid: 12088, cpu: 0.0, ws: 64 * 1048576, sig: 'valid', sigName: 'Adobe Inc.', verdict: 'safe', group: '后台服务', reasons: ['无窗口、无前台关联', '运行超 1 小时，零磁盘读写'] },
  { name: 'steamwebhelper.exe', pid: 7741, cpu: 0.2, ws: 190 * 1048576, sig: 'valid', sigName: 'Valve Corp.', verdict: 'keep', group: '游戏平台', reasons: ['有可见窗口，可能正在用'] },
  { name: 'SearchIndexer.exe', pid: 3210, cpu: 0.6, ws: 122 * 1048576, sig: 'valid', sigName: 'Microsoft Windows', verdict: 'forbidden', group: '系统组件', reasons: ['系统关键进程，结束会导致系统异常'] },
  { name: 'OneDriveStandaloneUpdater.exe', pid: 18044, cpu: 0.0, ws: 41 * 1048576, sig: 'valid', sigName: 'Microsoft Corporation', verdict: 'safe', group: '后台服务', reasons: ['无窗口、无前台关联', 'CPU 占用 0.0%，几乎无活动'] },
  { name: 'Music.UI.exe', pid: 21077, cpu: 0.0, ws: 96 * 1048576, sig: 'valid', sigName: 'Microsoft Corporation', verdict: 'safe', group: '后台服务', reasons: ['无窗口、无前台关联', 'CPU 占用 0.0%，几乎无活动'] },
  { name: 'svchost.exe', pid: 1288, cpu: 0.9, ws: 88 * 1048576, sig: 'valid', sigName: 'Microsoft Windows', verdict: 'forbidden', group: '系统组件', reasons: ['系统关键进程，结束会导致系统异常'] },
  { name: 'lsass.exe', pid: 712, cpu: 0.3, ws: 74 * 1048576, sig: 'valid', sigName: 'Microsoft Windows', verdict: 'forbidden', group: '系统组件', reasons: ['系统关键进程，结束会导致系统异常'] },
  { name: 'NVIDIA Share.exe', pid: 6109, cpu: 0.1, ws: 210 * 1048576, sig: 'valid', sigName: 'NVIDIA Corporation', verdict: 'keep', group: '后台服务', reasons: ['CPU 占用 0.1%，几乎无活动'] },
];

function mockScan() {
  const list = MOCK.map((m) => ({
    pid: m.pid, ppid: 1000, name: m.name, exe_path: 'C:\\Program Files\\' + m.name,
    uptime_secs: 5400, cpu_percent: m.cpu, working_set: m.ws, private_bytes: m.ws,
    thread_count: 12, handle_count: 180, io_read_bytes: 0, io_write_bytes: 0,
    session_id: 1, has_window: m.verdict === 'forbidden' && m.name !== 'SearchIndexer.exe' && m.name !== 'svchost.exe' && m.name !== 'lsass.exe',
    is_foreground: m.name === 'chrome.exe',
    signature: m.sig === 'valid' ? { valid: m.sigName } : 'unsigned',
    group: m.group,
    verdict: m.verdict,
    score: { activity: 90, foreground: 80, frequency: 70, signature: 60, criticality: 70, whitelist: 0 },
    reasons: m.reasons,
  }));
  return {
    ok: true,
    data: {
      processes: list,
      metrics: {
        cpu_percent: 14.2, mem_used: 9.8 * 1073741824, mem_total: 16 * 1073741824,
        disk_percent: 6.5, disk_read_bps: 1.2 * 1048576, disk_write_bps: 0.8 * 1048576,
        net_rx_bps: 3.4 * 1048576, net_tx_bps: 0.6 * 1048576,
        process_count: list.length, thread_count: 2841, uptime_secs: 182340,
      },
      stats: { process_count: list.length, enumerate_ms: 1.8, detail_ms: 12.4, total_ms: 14.2 },
    },
  };
}

/* ============================================================
   数据层
   ============================================================ */
async function api(cmd, args) {
  if (!IS_TAURI) {
    if (cmd === 'scan_processes') return mockScan();
    if (cmd === 'env_info') {
      return { ok: true, data: { version: '0.1.3', cpu_count: 16, pdh_healthy: true, portable: true, data_dir: 'D:\\old666\\data', admin: false, blacklist_coverage: [180, 44, 36] } };
    }
    if (cmd === 'whitelist_list') return { ok: true, data: state.whitelist };
    if (cmd === 'snapshot_list') return { ok: true, data: [] };
    return { ok: false, error: '预览模式：此操作需要真实运行环境' };
  }
  try {
    return await invoke(cmd, args || {});
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

/* ============================================================
   扫描与渲染
   ============================================================ */
async function scan(withPaths) {
  if (state.busy) return;
  state.busy = true;
  const t0 = performance.now();
  const r = await api('scan_processes', { withPaths: withPaths !== false });
  state.busy = false;

  if (!r || !r.ok) {
    if (r && r.error && !/预览模式/.test(r.error)) toast('扫描失败：' + r.error, true);
    return;
  }

  state.list = r.data.processes;
  state.metrics = r.data.metrics;
  state.stats = r.data.stats;

  // 清理已消失进程的选中态
  const alive = new Set(state.list.map((p) => p.pid));
  for (const pid of [...state.selected]) if (!alive.has(pid)) state.selected.delete(pid);

  renderMetrics();
  renderTable();
  renderCounts();
  updatePurgeHint();

  // 自适应刷新间隔：采集慢就放慢，保证 UI 永远跟手
  const cost = performance.now() - t0;
  if (cost > 300) state.interval = Math.min(5000, state.interval + 500);
  else if (cost < 90) state.interval = Math.max(1000, state.interval - 250);
  scheduleNext();
}

function scheduleNext() {
  clearTimeout(state.timer);
  if (document.hidden) return; // 窗口不可见时暂停采集，零打扰
  state.timer = setTimeout(() => {
    state.fullScanCounter++;
    // 每 5 轮做一次带路径的完整扫描（刷新签名信息），其余走轻量模式
    scan(state.fullScanCounter % 5 === 0);
  }, state.interval);
}

function renderMetrics() {
  const m = state.metrics;
  if (!m) return;

  const setBar = (el, pct) => {
    const i = el;
    i.style.width = Math.min(100, Math.max(0, pct)) + '%';
    i.className = pct >= 85 ? 'hot' : pct >= 65 ? 'warn' : '';
  };

  const cpu = m.cpu_percent || 0;
  $('#mCpu').textContent = cpu.toFixed(1) + '%';
  setBar($('#barCpu'), cpu);

  const memPct = m.mem_total ? (m.mem_used / m.mem_total) * 100 : 0;
  $('#mMem').textContent = memPct.toFixed(0) + '%';
  $('#mMem').title = humanBytes(m.mem_used) + ' / ' + humanBytes(m.mem_total);
  setBar($('#barMem'), memPct);

  const disk = m.disk_percent || 0;
  $('#mDisk').textContent = disk.toFixed(0) + '%';
  $('#mDisk').title = `读 ${humanRate(m.disk_read_bps)} · 写 ${humanRate(m.disk_write_bps)}`;
  setBar($('#barDisk'), disk);

  $('#mNet').textContent = humanRate((m.net_rx_bps || 0) + (m.net_tx_bps || 0));
  $('#mNetSub').textContent = `↑ ${humanRate(m.net_tx_bps)} ↓ ${humanRate(m.net_rx_bps)}`;

  $('#mProc').textContent = m.process_count ?? state.list.length;
  $('#mScanSub').textContent = state.stats
    ? `扫描 ${state.stats.total_ms.toFixed(1)} ms`
    : '扫描 —';
}

function verdictOf(p) { return p.verdict; }

function sigCell(p) {
  const s = p.signature;
  if (s && typeof s === 'object' && s.valid) {
    return `<span class="sig ok" title="${esc(s.valid)}">✓ ${esc(s.valid)}</span>`;
  }
  if (s === 'unsigned') return '<span class="sig no">无签名</span>';
  if (s === 'invalid') return '<span class="sig bad">签名无效</span>';
  return '<span class="sig">—</span>';
}

function verdictCell(v) {
  const map = { safe: ['safe', '可结束'], keep: ['keep', '建议保留'], forbidden: ['forbidden', '禁止结束'] };
  const [cls, label] = map[v] || ['', v];
  return `<span class="vd ${cls}">${label}</span>`;
}

function renderTable() {
  const tb = $('#tbody');
  const q = state.search.trim().toLowerCase();

  let list = state.list.filter((p) => {
    if (state.filter !== 'all' && p.verdict !== state.filter) return false;
    if (q && !p.name.toLowerCase().includes(q)) return false;
    return true;
  });

  if (!list.length) {
    tb.innerHTML = '';
    $('#empty').hidden = false;
    return;
  }
  $('#empty').hidden = true;

  // 分组：保持 verdict 排序的前提下，按 group 分块
  const groups = new Map();
  for (const p of list) {
    if (!groups.has(p.group)) groups.set(p.group, []);
    groups.get(p.group).push(p);
  }

  const maxWs = Math.max(...list.map((p) => p.working_set || 1), 1);
  let html = '';

  for (const [g, items] of groups) {
    html += `<tr class="group"><td colspan="8">${esc(g)} · ${items.length}</td></tr>`;
    for (const p of items) {
      const sel = state.selected.has(p.pid);
      const cpu = p.cpu_percent || 0;
      const cpuCls = cpu >= 25 ? 'hot' : cpu >= 8 ? 'warn' : '';
      const wsPct = (p.working_set / maxWs) * 100;
      const isSys = p.verdict === 'forbidden';

      html += `<tr class="proc ${isSys ? 'forbidden' : ''} ${sel ? 'sel' : ''}" data-pid="${p.pid}">
        <td class="c-chk"><label class="chk"><input type="checkbox" data-chk="${p.pid}" ${sel ? 'checked' : ''} ${isSys ? 'disabled' : ''}></label></td>
        <td class="c-name">
          <div class="pname">
            <span class="nm" title="${esc(p.exe_path || p.name)}">${esc(p.name)}</span>
            <span class="pid">${p.pid}</span>
            ${p.is_foreground ? '<span class="fg">前台</span>' : ''}
          </div>
        </td>
        <td class="c-cpu num">
          <div class="bar-cell">
            <span>${cpu.toFixed(1)}%</span>
            <span class="mini-bar"><i class="${cpuCls}" style="width:${Math.min(100, cpu * 4)}%"></i></span>
          </div>
        </td>
        <td class="c-mem num">
          <div class="bar-cell">
            <span>${humanBytes(p.working_set)}</span>
            <span class="mini-bar"><i style="width:${wsPct}%"></i></span>
          </div>
        </td>
        <td class="c-io num" title="累计读写">${humanBytes((p.io_read_bytes || 0) + (p.io_write_bytes || 0))}</td>
        <td class="c-sig">${sigCell(p)}</td>
        <td class="c-verdict">${verdictCell(p.verdict)}</td>
        <td class="c-act">
          ${isSys ? '' : `<button class="act warn" data-wl="${esc(p.name)}" title="加入白名单，永久保留">保留</button>`}
        </td>
      </tr>`;
    }
  }

  tb.innerHTML = html;
}

function renderCounts() {
  let safe = 0, keep = 0, forb = 0;
  for (const p of state.list) {
    if (p.verdict === 'safe') safe++;
    else if (p.verdict === 'keep') keep++;
    else forb++;
  }
  $('#cntSafe').textContent = safe;
  $('#cntKeep').textContent = keep;
  $('#cntForbidden').textContent = forb;
}

function updatePurgeHint() {
  const safe = state.list.filter((p) => p.verdict === 'safe');
  const mem = safe.reduce((a, p) => a + (p.working_set || 0), 0);
  const btn = $('#btnPurge');
  if (!state.list.length) {
    $('#purgeHint').textContent = '正在分析…';
    btn.disabled = true;
    return;
  }
  btn.disabled = false;
  $('#purgeHint').textContent = safe.length
    ? `发现 ${safe.length} 个闲置进程，可释放约 ${humanBytes(mem)}`
    : '当前没有可净化的闲置进程，很干净';
}

/* ============================================================
   净化
   ============================================================ */
async function doPurge(onlyTrim) {
  if (!IS_TAURI) { toast('预览模式：需要真实运行才能净化', true); return; }

  const btn = $('#btnPurge');
  btn.disabled = true;
  $('#purgeHint').textContent = '正在净化…';

  // 用户勾选了具体进程 → 只处理这些；否则自动模式
  const targets = state.selected.size > 0 ? [...state.selected] : null;
  const r = await api('purge_processes', { targets, graceMs: onlyTrim ? 0 : 800 });

  btn.disabled = false;

  if (!r || !r.ok) {
    toast('净化失败：' + (r && r.error), true);
    $('#purgeHint').textContent = '净化失败';
    return;
  }

  const res = r.data;
  if (onlyTrim) {
    // 只释放内存：把 killed 换成 trimmed 的语义
    res.killed = [];
  }

  showResult(res);
  state.selected.clear();
  $('#chkAll').checked = false;
  await scan(true);
  loadSnapshots();
}

function showResult(res) {
  const killed = (res.killed || []).filter((k) => !k.error).length;
  const trimmed = (res.trimmed || []).filter((k) => !k.error).length;
  const blocked = (res.blocked || []).length;

  $('#resultTitle').textContent = killed || trimmed ? '净化完成' : '系统已经很干净';
  $('#resultStats').innerHTML = `
    <div class="rs"><b>${humanBytes(res.freed_bytes)}</b><span>释放内存</span></div>
    <div class="rs"><b>${killed}</b><span>结束进程</span></div>
    <div class="rs"><b>${trimmed}</b><span>内存瘦身</span></div>
    <div class="rs"><b>${res.elapsed_ms} ms</b><span>耗时</span></div>
    ${blocked ? `<div class="rs"><b>${blocked}</b><span>安全拦截</span></div>` : ''}
  `;

  let detail = '';
  const names = (res.killed || []).filter((k) => !k.error).map((k) => k.name);
  if (names.length) {
    detail += '<div>已结束：' + names.slice(0, 12).map((n) => `<span class="tagline">${esc(n)}</span>`).join('') +
      (names.length > 12 ? `<span class="tagline">…等 ${names.length} 个</span>` : '') + '</div>';
  }
  if (blocked) {
    const bn = (res.blocked || []).map((k) => k.name);
    detail += '<div style="margin-top:5px">出于安全挡下：' +
      bn.slice(0, 8).map((n) => `<span class="tagline blocked">${esc(n)}</span>`).join('') + '</div>';
  }
  const failed = (res.killed || []).filter((k) => k.error);
  if (failed.length) {
    detail += `<div style="margin-top:5px">未能结束 ${failed.length} 个（多为权限不足，可尝试以管理员身份重启）</div>`;
  }
  $('#resultDetail').innerHTML = detail;
  $('#result').hidden = false;
}

async function doRollback(id) {
  if (!IS_TAURI) { toast('预览模式：无法回滚', true); return; }
  const r = await api('snapshot_rollback', { id });
  if (!r || !r.ok) { toast('回滚失败：' + (r && r.error), true); return; }
  const o = r.data;
  toast(`已重新拉起 ${o.ok} 个进程${o.fail ? `，${o.fail} 个失败` : ''}`, o.fail > 0);
  if (o.errors && o.errors.length) console.warn('回滚失败明细', o.errors);
  loadSnapshots();
}

/* ============================================================
   白名单 / 快照
   ============================================================ */
async function loadWhitelist() {
  const r = await api('whitelist_list');
  if (r && r.ok) {
    state.whitelist = r.data;
    renderWhitelist();
  }
}

function renderWhitelist() {
  const ul = $('#wlList');
  const names = state.whitelist.names || [];
  if (!names.length) {
    ul.innerHTML = '<li class="empty">暂无白名单</li>';
    return;
  }
  ul.innerHTML = names.map((n) =>
    `<li><span>${esc(n)}</span><button class="act" data-wlrm="${esc(n)}">移除</button></li>`).join('');
}

async function addWhitelist(name) {
  if (!name) return;
  if (!IS_TAURI) { toast('预览模式：无法保存白名单', true); return; }
  const r = await api('whitelist_add', { name, path: null });
  if (!r || !r.ok) { toast(r && r.error ? r.error : '加入失败', true); return; }
  state.whitelist = r.data;
  renderWhitelist();
  toast(`已加入白名单：${name}`);
  scan(true);
}

async function removeWhitelist(name) {
  const r = await api('whitelist_remove', { name });
  if (!r || !r.ok) { toast('移除失败', true); return; }
  state.whitelist = r.data;
  renderWhitelist();
  toast(`已移出白名单：${name}`);
  scan(true);
}

async function loadSnapshots() {
  const r = await api('snapshot_list');
  if (!r || !r.ok) return;
  state.snapshots = (r.data || []).slice().reverse();
  const ul = $('#snapList');
  if (!state.snapshots.length) {
    ul.innerHTML = '<li class="empty">暂无净化记录</li>';
    return;
  }
  ul.innerHTML = state.snapshots.slice(0, 10).map((s) => {
    const when = new Date(s.timestamp * 1000).toLocaleString('zh-CN', { hour12: false });
    const n = (s.killed || []).filter((k) => !k.error).length;
    const rollbackable = (s.killed || []).filter((k) => !k.error && k.method !== 'skipped' && k.method !== 'gone').length;
    return `<li>
      <div class="snap-top">
        <b>释放 ${humanBytes(s.freed_bytes)}</b>
        <time>${esc(when)}</time>
      </div>
      <div class="snap-sub">结束 ${n} 个进程${rollbackable ? ` · 可回滚 ${rollbackable} 个` : ''}</div>
      ${rollbackable ? `<button class="btn soft sm" data-rb="${esc(s.id)}">撤销这次净化</button>` : ''}
    </li>`;
  }).join('');
}

/* ============================================================
   环境信息
   ============================================================ */
async function loadEnv() {
  const r = await api('env_info');
  if (!r || !r.ok) return;
  const e = r.data;
  $('#verText').textContent = e.version;

  const dot = $('#envDot'), txt = $('#envText');
  if (!IS_TAURI) {
    dot.className = 'dot warn';
    txt.textContent = '界面预览模式';
  } else if (e.admin) {
    dot.className = 'dot on';
    txt.textContent = '管理员模式';
  } else {
    dot.className = 'dot warn';
    txt.textContent = '普通权限';
    $('#btnElevate').hidden = false;
  }

  $('#envDl').innerHTML = `
    <dt>版本</dt><dd>${esc(e.version)}</dd>
    <dt>逻辑处理器</dt><dd>${e.cpu_count}</dd>
    <dt>性能计数器</dt><dd>${e.pdh_healthy ? '可用' : '不可用'}</dd>
    <dt>管理员权限</dt><dd>${e.admin ? '是' : '否'}</dd>
    <dt>便携模式</dt><dd>${e.portable ? '是' : '否'}</dd>
    <dt>数据目录</dt><dd>${esc(e.data_dir)}</dd>
    <dt>黑名单覆盖</dt><dd>${e.blacklist_coverage.join(' / ')}</dd>
  `;
}

/* ============================================================
   事件绑定
   ============================================================ */
function bind() {
  // 筛选
  document.querySelectorAll('.chip').forEach((c) => {
    c.addEventListener('click', () => {
      document.querySelectorAll('.chip').forEach((x) => x.classList.remove('active'));
      c.classList.add('active');
      state.filter = c.dataset.filter;
      renderTable();
    });
  });

  // 搜索
  $('#search').addEventListener('input', (e) => {
    state.search = e.target.value;
    renderTable();
  });

  // 全选（只选可结束的）
  $('#chkAll').addEventListener('change', (e) => {
    state.selected.clear();
    if (e.target.checked) {
      state.list.filter((p) => p.verdict !== 'forbidden').forEach((p) => state.selected.add(p.pid));
    }
    renderTable();
  });

  // 表格内交互（事件委托）
  $('#tbody').addEventListener('click', (e) => {
    const wl = e.target.closest('[data-wl]');
    if (wl) { addWhitelist(wl.dataset.wl); return; }

    const chk = e.target.closest('[data-chk]');
    if (chk) {
      const pid = Number(chk.dataset.chk);
      if (chk.checked) state.selected.add(pid); else state.selected.delete(pid);
      chk.closest('tr').classList.toggle('sel', chk.checked);
      return;
    }

    const row = e.target.closest('tr.proc');
    if (row && !e.target.closest('button')) {
      const cb = row.querySelector('input[data-chk]');
      if (cb && !cb.disabled) { cb.checked = !cb.checked; cb.dispatchEvent(new Event('change', { bubbles: true })); }
    }
  });

  $('#tbody').addEventListener('change', (e) => {
    const chk = e.target.closest('[data-chk]');
    if (!chk) return;
    const pid = Number(chk.dataset.chk);
    if (chk.checked) state.selected.add(pid); else state.selected.delete(pid);
    chk.closest('tr').classList.toggle('sel', chk.checked);
  });

  // 净化
  $('#btnPurge').addEventListener('click', () => doPurge(false));
  $('#btnTrimOnly').addEventListener('click', () => doPurge(true));

  // 撤销
  $('#btnRollback').addEventListener('click', () => {
    if (state.snapshots.length) doRollback(state.snapshots[0].id);
    else toast('还没有可撤销的净化记录');
  });

  // 刷新
  $('#btnRefresh').addEventListener('click', () => scan(true));

  // 提权
  $('#btnElevate').addEventListener('click', async () => {
    if (!IS_TAURI) { toast('预览模式'); return; }
    toast('正在请求管理员权限…');
    // 由后端拉起提权实例；成功后当前窗口应关闭
    try { await invoke('plugin:process|restart'); } catch (_) {
      toast('请在 Windows 上右键以管理员身份运行', true);
    }
  });

  // 结果关闭
  $('#resultClose').addEventListener('click', () => { $('#result').hidden = true; });

  // 抽屉
  $('#drawerMask').addEventListener('click', closeDrawer);
  $('#drawerClose').addEventListener('click', closeDrawer);
  $('#wlAdd').addEventListener('click', () => {
    const v = $('#wlInput').value.trim();
    if (v) { addWhitelist(v); $('#wlInput').value = ''; }
  });
  $('#wlInput').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { const v = e.target.value.trim(); if (v) { addWhitelist(v); e.target.value = ''; } }
  });
  $('#wlList').addEventListener('click', (e) => {
    const b = e.target.closest('[data-wlrm]');
    if (b) removeWhitelist(b.dataset.wlrm);
  });
  $('#snapList').addEventListener('click', (e) => {
    const b = e.target.closest('[data-rb]');
    if (b) doRollback(b.dataset.rb);
  });

  // 导航
  document.querySelectorAll('.nav-item').forEach((n) => {
    n.addEventListener('click', () => {
      if (n.disabled) return;
      if (n.dataset.view === 'settings') { openDrawer(); return; }
      document.querySelectorAll('.nav-item').forEach((x) => x.classList.remove('active'));
      n.classList.add('active');
      switchView(n.dataset.view);
    });
  });

  // 窗口可见性：切走就停止采集，省电
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) clearTimeout(state.timer);
    else scan(true);
  });
}

function openDrawer() {
  $('#drawerMask').hidden = false;
  $('#drawer').hidden = false;
  loadWhitelist();
  loadSnapshots();
  // 知识库与启动项名单在 boot.js 里 —— 脚本加载顺序保证它此时已经就位，
  // 但预览模式下 boot.js 只声明不执行，这里仍做一次存在性判断。
  if (typeof loadSettingsExtras === 'function') loadSettingsExtras();
}

/* ============================================================
   视图切换（P2 起支持多页）
   ------------------------------------------------------------
   切到开机加速时**停掉**进程刷新的定时器：那一页完全不看进程数据，
   继续采集只是白占 CPU 与风扇。
   ============================================================ */
const VIEW_META = {
  purge: {
    title: '进程净化',
    sub: '识别并结束闲置进程，释放被占用的内存',
    refresh: true,
  },
  boot: {
    title: '开机加速',
    sub: '看清开机时间花在哪，关掉不必要的自启动项',
    refresh: false,
  },
  mode: {
    title: '场景模式',
    sub: '一套规则一个场景：日常、游戏、服务器，一键切换',
    refresh: false,
  },
};

function switchView(name) {
  const meta = VIEW_META[name] || VIEW_META.purge;
  ['purge', 'boot', 'mode'].forEach((v) => {
    const el = document.getElementById('view' + v[0].toUpperCase() + v.slice(1));
    if (el) el.hidden = v !== name;
  });
  $('#viewTitle').textContent = meta.title;
  $('#viewSub').textContent = meta.sub;
  $('#btnRefresh').hidden = !meta.refresh;

  // 进程页才需要持续采样；开机加速页与场景模式页没有实时数据要看
  if (!meta.refresh) {
    clearTimeout(state.timer);
    state.timer = null;
  }

  if (name === 'boot' && typeof enterBootView === 'function') {
    enterBootView();
  } else if (name === 'mode' && typeof enterModeView === 'function') {
    enterModeView();
  } else if (name === 'purge' && !state.timer) {
    scan(true);
  }
}

function closeDrawer() {
  $('#drawerMask').hidden = true;
  $('#drawer').hidden = true;
}

/* ============================================================
   启动
   ============================================================ */
async function init() {
  bind();
  await loadEnv();
  await scan(true);          // 首轮带路径，拿到签名
  await loadSnapshots();
}

init();
