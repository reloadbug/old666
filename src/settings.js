/* ============================================================
   老六清净 old666 · 设置抽屉里的「崩溃日志」与「更新」
   ------------------------------------------------------------
   两条硬约束在代码里也要看得见：
   1. 崩溃日志**只写本机**，这里只负责列出来给你看、让你删。
   2. 自动检查更新**默认关闭**，开启后也只提示、不下载、不安装。
   ============================================================ */

(function () {
  const IS_TAURI = typeof window.__TAURI__ !== 'undefined' && !!window.__TAURI__.core;
  const invoke = IS_TAURI ? window.__TAURI__.core.invoke : null;

  const $s = (sel) => document.querySelector(sel);
  const sState = { crashes: [], selected: '', upd: null };

  function sToast(msg, isErr) {
    const el = document.querySelector('#toast');
    if (!el) return;
    el.textContent = msg;
    el.className = 'toast' + (isErr ? ' err' : '');
    el.hidden = false;
    clearTimeout(el._t);
    el._t = setTimeout(() => { el.hidden = true; }, 2600);
  }

  function sEsc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, (c) =>
      ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }

  async function sCall(cmd, args) {
    if (!IS_TAURI) return { ok: false, error: '预览模式：此操作需要真实运行环境' };
    try {
      return await invoke(cmd, args || {});
    } catch (e) {
      return { ok: false, error: String(e) };
    }
  }

  function sHumanBytes(b) {
    if (!b) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB'];
    let v = b, i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return (i === 0 ? v.toFixed(0) : v.toFixed(1)) + ' ' + u[i];
  }

  function sTime(sec) {
    if (!sec) return '';
    const d = new Date(sec * 1000);
    const p = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  /* ---------------------- 崩溃日志 ---------------------- */

  async function loadCrashLogs() {
    const r = await sCall('crash_list');
    const ul = $s('#crashList');
    if (!ul) return;
    if (!r || !r.ok) {
      ul.innerHTML = '<li class="hint">暂时读不到崩溃日志（预览模式或日志目录不可读）。</li>';
      return;
    }
    sState.crashes = r.data || [];
    if (!sState.crashes.length) {
      ul.innerHTML = '<li class="hint">没有崩溃记录 —— 挺好，说明程序一直安安稳稳。</li>';
      return;
    }
    ul.innerHTML = sState.crashes.map((c) => `
      <li class="crash-item" data-crash="${sEsc(c.file)}">
        <span class="crash-when">${sEsc(sTime(c.modified))}</span>
        <span class="crash-summary" title="${sEsc(c.summary)}">${sEsc(c.summary || '（无消息）')}</span>
        <span class="crash-size">${sEsc(sHumanBytes(c.size))}</span>
      </li>`).join('');
  }

  async function readCrash() {
    if (!sState.selected) { sToast('先在列表里点一条崩溃记录', true); return; }
    const r = await sCall('crash_read', { file: sState.selected });
    if (!r || !r.ok) { sToast(r && r.error ? r.error : '读取失败', true); return; }
    sToast('已读取，内容可在日志目录里直接查看');
    // 原文太长时不弹窗，落到日志目录更实际。
    const dir = await sCall('crash_logs_dir');
    if (dir && dir.ok) sToast('日志目录：' + dir.data);
  }

  async function clearCrashes() {
    const r = await sCall('crash_clear');
    if (!r || !r.ok) { sToast(r && r.error ? r.error : '清空失败', true); return; }
    sState.selected = '';
    sToast(`已清空 ${r.data} 份崩溃日志`);
    await loadCrashLogs();
  }

  /* ---------------------- 更新 ---------------------- */

  async function loadUpdateConfig() {
    const r = await sCall('update_config');
    if (!r || !r.ok) return;
    const cfg = r.data || { enabled: false, url: '' };
    sState.upd = cfg;
    const en = $s('#updEnabled');
    const url = $s('#updUrl');
    if (en) en.value = cfg.enabled ? '1' : '0';
    if (url) url.value = cfg.url || '';
    const st = $s('#updState');
    if (st) {
      st.textContent = cfg.enabled
        ? '已开启：启动时会检查一次，只提示有新版本，不会自动下载或安装。'
        : '当前已关闭：程序不会发起任何更新相关的网络请求。';
    }
    const vt = $s('#verText');
    if (vt && window.__TAURI__ && window.__TAURI__.app) {
      try {
        const v = await window.__TAURI__.app.getVersion();
        vt.value = v;
      } catch (_) { /* 拿不到就留空 */ }
    }
  }

  async function saveUpdateConfig() {
    const en = $s('#updEnabled');
    const url = $s('#updUrl');
    const cfg = {
      enabled: en && en.value === '1',
      url: url ? url.value.trim() : '',
    };
    const r = await sCall('update_config_save', { config: cfg });
    if (!r || !r.ok) { sToast(r && r.error ? r.error : '保存失败', true); return; }
    sState.upd = cfg;
    sToast(cfg.enabled ? '已开启更新检查（只提示）' : '已关闭更新检查');
    await loadUpdateConfig();
  }

  async function checkUpdate() {
    const r = await sCall('update_check');
    const st = $s('#updState');
    if (!r || !r.ok) { sToast(r && r.error ? r.error : '检查失败', true); return; }
    const s = r.data || {};
    if (s.skipped) {
      if (st) st.textContent = '更新检查已关闭，没有发出任何网络请求。';
      sToast('更新检查已关闭');
      return;
    }
    if (s.error) {
      if (st) st.textContent = '检查失败：' + s.error;
      sToast('检查失败', true);
      return;
    }
    if (s.has_update) {
      if (st) st.textContent = `发现新版本 ${s.latest}（当前 ${s.current}）。下载地址：${s.download_url || '见发布页'}。不会自动下载或安装。`;
      sToast('发现新版本 ' + s.latest);
    } else {
      if (st) st.textContent = `已是最新版本（${s.current}）。`;
      sToast('已是最新版本');
    }
  }

  /* ---------------------- 绑定 ---------------------- */

  function bindSettings() {
    const list = $s('#crashList');
    if (list && !list._bound) {
      list._bound = true;
      list.addEventListener('click', (e) => {
        const it = e.target.closest('[data-crash]');
        if (!it) return;
        sState.selected = it.dataset.crash;
        list.querySelectorAll('.crash-item').forEach((x) => x.classList.remove('on'));
        it.classList.add('on');
      });
    }

    const rd = $s('#btnCrashRead');
    if (rd && !rd._bound) { rd._bound = true; rd.addEventListener('click', readCrash); }

    const cl = $s('#btnCrashClear');
    if (cl && !cl._bound) { cl._bound = true; cl.addEventListener('click', clearCrashes); }

    const us = $s('#btnUpdSave');
    if (us && !us._bound) { us._bound = true; us.addEventListener('click', saveUpdateConfig); }

    const uc = $s('#btnUpdCheck');
    if (uc && !uc._bound) { uc._bound = true; uc.addEventListener('click', checkUpdate); }
  }

  // boot.js 里已经有一个 loadSettingsExtras（加载知识库与启动项名单）。
  // 别把它顶掉 —— 接在它后面一起跑。
  const prevExtras = window.loadSettingsExtras;

  async function loadSettingsExtras() {
    bindSettings();
    await Promise.all([
      prevExtras ? prevExtras() : Promise.resolve(),
      loadCrashLogs(),
      loadUpdateConfig(),
    ]);
  }

  window.loadSettingsExtras = loadSettingsExtras;
  window.loadCrashLogs = loadCrashLogs;
  window.loadUpdateConfig = loadUpdateConfig;

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bindSettings);
  } else {
    bindSettings();
  }
})();
