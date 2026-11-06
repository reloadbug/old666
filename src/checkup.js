/* ============================================================
   P4 · 一键体检 + 垃圾清理
   ------------------------------------------------------------
   设计要点：
   - 体检只看不改。分数与理由都由后端算好，这里只负责显示。
   - 垃圾清理分两步：先扫描（只读），再由用户勾选、确认，才送回收站。
   - 「送进回收站」之前必须弹确认框，列出将要清理的清单。
   ============================================================ */

(function () {
  const $c = (s) => document.querySelector(s);
  const cEsc = (s) =>
    String(s == null ? '' : s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');

  const IS_TAURI = typeof window !== 'undefined' && window.__TAURI__ && window.__TAURI__.core;
  const cInvoke = IS_TAURI ? window.__TAURI__.core.invoke : null;

  async function cCall(cmd, args) {
    const raw = await cInvoke(cmd, args || {});
    if (raw && typeof raw === 'object' && 'ok' in raw) {
      if (!raw.ok) throw new Error(raw.error || '操作失败');
      return raw.data;
    }
    return raw;
  }

  function cToast(msg, isErr) {
    const t = document.getElementById('toast');
    if (!t) return;
    t.textContent = msg;
    t.className = 'toast' + (isErr ? ' err' : '');
    t.hidden = false;
    clearTimeout(t._t);
    t._t = setTimeout(() => { t.hidden = true; }, 2600);
  }

  function cBytes(b) {
    if (!b) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let n = b;
    while (n >= 1024 && i < u.length - 1) {
      n /= 1024;
      i++;
    }
    return (i === 0 ? n : n.toFixed(1)) + ' ' + u[i];
  }

  const SEV_LABEL = {
    good: '良好',
    notice: '留意',
    warn: '建议处理',
    bad: '严重',
    unknown: '无法判定',
  };

  const KIND_LABEL = {
    user_temp: '用户临时文件',
    windows_temp: '系统临时文件',
    thumbnail_cache: '缩略图缓存',
    update_residue: '更新残留',
    crash_dump: '崩溃转储文件',
    recycle_bin: '回收站内容',
  };

  /** 条目名：后端只给完整路径，这里取末段文件名。 */
  function cName(it) {
    if (!it) return '';
    const p = String(it.path || '');
    const parts = p.split(/[\\/]/);
    return parts[parts.length - 1] || p;
  }

  const cState = {
    report: null,
    junk: null,
    selected: new Set(),
  };

  /* ---------------------- 体检 ---------------------- */

  function renderCheckup(report) {
    cState.report = report;
    $('#checkupScore').textContent = report ? String(report.score) : '—';
    const sub = $('#checkupSub');
    if (report) {
      sub.innerHTML =
        cEsc(report.summary) +
        ' · 耗时 ' +
        report.elapsed_ms +
        ' ms' +
        (report.needs_admin ? '' : ' · 部分结果需要管理员权限才完整');
    }

    const list = $('#checkupList');
    if (!report) {
      list.innerHTML = '';
      return;
    }
    list.innerHTML = report.items
      .map((it) => {
        const sev = it.severity || 'good';
        return (
          '<li class="checkup-item ' +
          cEsc(sev) +
          '">' +
          '<div class="checkup-item-head">' +
          '<span class="checkup-item-title">' +
          cEsc(it.title) +
          '</span>' +
          '<span class="checkup-item-sev">' +
          cEsc(SEV_LABEL[sev] || sev) +
          '</span>' +
          '</div>' +
          '<div class="checkup-item-reason">' +
          cEsc(it.reason) +
          '</div>' +
          (it.metric ? '<div class="checkup-item-metric">' + cEsc(it.metric) + '</div>' : '') +
          (it.advice ? '<div class="checkup-item-advice">建议：' + cEsc(it.advice) + '</div>' : '') +
          '</li>'
        );
      })
      .join('');
  }

  async function runCheckup() {
    const btn = $('#btnCheckupRun');
    if (btn) btn.disabled = true;
    try {
      const r = await cCall('checkup_run');
      renderCheckup(r);
      cToast('体检完成');
    } catch (e) {
      cToast('体检失败：' + e, true);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  /* ---------------------- 垃圾清理 ---------------------- */

  function renderJunkGroups(scan) {
    const box = $('#junkGroups');
    if (!scan) {
      box.innerHTML = '';
      return;
    }
    if (!scan.groups.length) {
      box.innerHTML = '<div class="junk-empty">没扫到可清理的垃圾，系统挺干净的。</div>';
      return;
    }
    box.innerHTML = scan.groups
      .map((g) => {
        const key = g[0];
        const n = g[1];
        const bytes = g[2];
        const deletable = key !== 'recycle_bin';
        return (
          '<div class="junk-group' +
          (deletable ? '' : ' readonly') +
          '">' +
          '<span class="junk-group-name">' +
          cEsc(KIND_LABEL[key] || key) +
          '</span>' +
          '<span class="junk-group-count">' +
          n +
          ' 项</span>' +
          '<span class="junk-group-size">' +
          cEsc(cBytes(bytes)) +
          '</span>' +
          '</div>'
        );
      })
      .join('');
  }

  function renderJunkItems() {
    const scan = cState.junk;
    const box = $('#junkItems');
    if (!scan || !box) return;
    const list = scan.items.filter((i) => i.kind !== 'recycle_bin');
    if (!list.length) {
      box.innerHTML = '<div class="junk-empty">没有可清理的条目。</div>';
      return;
    }
    box.innerHTML = list
      .map((it) => {
        const on = cState.selected.has(it.path);
        return (
          '<label class="junk-item' +
          (on ? ' on' : '') +
          '">' +
          '<input type="checkbox" data-path="' +
          cEsc(it.path) +
          '"' +
          (on ? ' checked' : '') +
          '>' +
          '<span class="junk-item-name" title="' +
          cEsc(it.path) +
          '">' +
          cEsc(cName(it)) +
          '</span>' +
          '<span class="junk-item-kind">' +
          cEsc(KIND_LABEL[it.kind] || it.kind) +
          '</span>' +
          '<span class="junk-item-size">' +
          cEsc(cBytes(it.size)) +
          '</span>' +
          '</label>'
        );
      })
      .join('');
  }

  function refreshCleanButton() {
    const btn = $('#btnJunkClean');
    if (btn) {
      btn.disabled = cState.selected.size === 0;
      btn.textContent = cState.selected.size
        ? '送进回收站（' + cState.selected.size + '）'
        : '送进回收站';
    }
  }

  async function scanJunk() {
    const btn = $('#btnJunkScan');
    if (btn) btn.disabled = true;
    try {
      const s = await cCall('junk_scan');
      cState.junk = s;
      cState.selected = new Set();
      renderJunkGroups(s);
      renderJunkItems();
      refreshCleanButton();
      const detail = $('#junkDetail');
      if (detail) detail.hidden = false;
      const sub = $('#junkSub');
      if (sub) {
        sub.innerHTML =
          '共找到 <b>' +
          cEsc(cBytes(s.total_bytes)) +
          '</b>（' +
          s.items.length +
          ' 项），耗时 ' +
          s.elapsed_ms +
          ' ms。只扫描系统临时目录与缓存，<b>绝不碰</b>桌面、文档、图片、下载。';
      }
      const foot = $('#junkFoot');
      if (foot && s.skipped && s.skipped.length) {
        foot.textContent = '有 ' + s.skipped.length + ' 个目录因权限不足被跳过：' + s.skipped[0];
      }
      cToast('扫描完成');
    } catch (e) {
      cToast('扫描失败：' + e, true);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  /** 弹确认框，列出将要清理的清单。返回 Promise<boolean>。 */
  function confirmClean(list) {
    return new Promise((resolve) => {
      const mask = $('#confirmMask');
      $('#confirmTitle').textContent = '确认把这些文件送进回收站？';
      $('#confirmLead').textContent =
        '共 ' +
        list.length +
        ' 个文件将被移动到系统回收站。';
      $('#confirmList').innerHTML = list
        .slice(0, 200)
        .map((it) => '<li>' + cEsc(cName(it)) + ' — ' + cEsc(cBytes(it.size)) + '</li>')
        .join('');
      if (list.length > 200) {
        $('#confirmList').innerHTML +=
          '<li>… 还有 ' + (list.length - 200) + ' 项未列出</li>';
      }
      $('#confirmHint').innerHTML =
        '文件进的是<b>系统回收站</b>，不会物理删除，你随时可以从回收站恢复。';

      const ok = $('#confirmOk');
      const cancel = $('#confirmCancel');
      mask.hidden = false;

      function cleanup(res) {
        mask.hidden = true;
        ok.removeEventListener('click', onOk);
        cancel.removeEventListener('click', onCancel);
        resolve(res);
      }
      function onOk() {
        cleanup(true);
      }
      function onCancel() {
        cleanup(false);
      }
      ok.addEventListener('click', onOk);
      cancel.addEventListener('click', onCancel);
    });
  }

  async function cleanJunk() {
    const scan = cState.junk;
    if (!scan || cState.selected.size === 0) return;
    const list = scan.items.filter((i) => cState.selected.has(i.path));
    const yes = await confirmClean(list);
    if (!yes) {
      cToast('已取消，一个文件都没动');
      return;
    }
    try {
      const r = await cCall('junk_delete', { paths: list.map((i) => i.path) });
      cToast(
        '已送进回收站 ' + r.deleted + ' 项' + (r.failed ? '，失败 ' + r.failed + ' 项' : ''),
        r.failed > 0
      );
      await scanJunk();
    } catch (e) {
      cToast('清理失败：' + e, true);
    }
  }

  /* ---------------------- 一键修复 + 前后对比 ---------------------- */

  function renderFixSummary(s) {
    const card = $('#fixCompareCard');
    const body = $('#fixCompareBody');
    const sub = $('#fixCompareSub');
    if (!card || !body) return;

    const delta = s.score_delta;
    const deltaTxt = delta > 0 ? '+' + delta : String(delta);
    const memBefore = Math.round(s.before_mem_pct);
    const memAfter = Math.round(s.after_mem_pct);
    const memDelta = memAfter - memBefore;

    sub.innerHTML =
      '健康分 ' +
      s.before_score +
      ' → <b>' +
      s.after_score +
      '</b>（' +
      deltaTxt +
      '）｜ 内存占用 ' +
      memBefore +
      '% → ' +
      memAfter +
      '%（' +
      (memDelta > 0 ? '+' : '') +
      memDelta +
      ' 个百分点）｜ 释放 ' +
      cEsc(cBytes(s.freed_bytes)) +
      ' ｜ 耗时 ' +
      s.elapsed_ms +
      ' ms';

    const rows = (s.outcomes || [])
      .map((o) => {
        return (
          '<div class="fix-row' +
          (o.ok ? '' : ' skipped') +
          '">' +
          '<span class="fix-row-kind">' +
          cEsc(o.kind) +
          '</span>' +
          '<span class="fix-row-detail">' +
          cEsc(o.detail) +
          '</span>' +
          '<span class="fix-row-bytes">' +
          cEsc(cBytes(o.freed_bytes)) +
          '</span>' +
          '</div>'
        );
      })
      .join('');

    body.innerHTML =
      (rows || '<div class="junk-empty">没有执行任何修复动作。</div>') +
      '<div class="fix-note">一键修复<b>不会结束任何进程</b>；只做工作集裁剪和「送进回收站」这两类可逆动作。' +
      '要结束进程，请到「净化」页，那里有逐次确认。</div>';

    card.hidden = false;
  }

  async function fixAll() {
    const btn = $('#btnCheckupFixAll');
    if (btn) btn.disabled = true;
    try {
      /* 只修「安全项」：裁剪工作集（不结束进程）。
         垃圾清理要用户逐项勾选，不在这里替用户决定。 */
      const s = await cCall('checkup_fix', {
        requests: [{ kind: 'trim_memory', paths: [] }],
      });
      renderFixSummary(s);
      if (s.after) renderCheckup(s.after);
      cToast('修复完成：健康分 ' + s.before_score + ' → ' + s.after_score);
    } catch (e) {
      cToast('修复失败：' + e, true);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  /* ---------------------- 绑定 ---------------------- */

  function bindCheckup() {
    const runBtn = $('#btnCheckupRun');
    if (runBtn) runBtn.addEventListener('click', runCheckup);

    const fixBtn = $('#btnCheckupFixAll');
    if (fixBtn) fixBtn.addEventListener('click', fixAll);

    const scanBtn = $('#btnJunkScan');
    if (scanBtn) scanBtn.addEventListener('click', scanJunk);

    const cleanBtn = $('#btnJunkClean');
    if (cleanBtn) cleanBtn.addEventListener('click', cleanJunk);

    const itemsBox = $('#junkItems');
    if (itemsBox) {
      itemsBox.addEventListener('change', (e) => {
        const cb = e.target;
        if (!cb || cb.type !== 'checkbox') return;
        const p = cb.getAttribute('data-path');
        if (!p) return;
        if (cb.checked) cState.selected.add(p);
        else cState.selected.delete(p);
        const row = cb.closest('.junk-item');
        if (row) row.classList.toggle('on', cb.checked);
        refreshCleanButton();
      });
    }

    const all = $('#junkSelectAll');
    if (all) {
      all.addEventListener('change', () => {
        const scan = cState.junk;
        if (!scan) return;
        const list = scan.items.filter((i) => i.kind !== 'recycle_bin');
        cState.selected = new Set();
        if (all.checked) list.forEach((i) => cState.selected.add(i.path));
        renderJunkItems();
        refreshCleanButton();
      });
    }
  }

  async function enterCheckupView() {
    if (!cState.report) await runCheckup();
  }

  window.enterCheckupView = enterCheckupView;
  window.renderCheckupReport = renderCheckup;

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bindCheckup);
  } else {
    bindCheckup();
  }
})();
