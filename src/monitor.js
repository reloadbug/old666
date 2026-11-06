/* P5 · 实时监控 —— 常驻采样、阈值告警、悬浮球控制。
 *
 * 设计约束：
 * 1. 常驻监控默认**开着**，但采样在后台线程里做，前端只是每 3 秒拉一次快照。
 * 2. 告警「轻提示」：不弹窗、不抢焦点，只在列表里出现一条。
 * 3. 监控页**不提供任何结束进程的入口** —— 那是「净化」页的事，
 *    而且那边必须逐次确认。
 */
(function () {
  'use strict';

  var $m = function (s) { return document.querySelector(s); };
  var IS_TAURI = !!(window.__TAURI__ && window.__TAURI__.core);
  var mState = { snap: null, timer: null, fb: null };

  function mInvoke(cmd, args) {
    if (!IS_TAURI) return Promise.reject(new Error('预览模式'));
    return window.__TAURI__.core.invoke(cmd, args || {});
  }

  function mCall(cmd, args) {
    return mInvoke(cmd, args).then(function (r) {
      if (r && r.ok === false) throw new Error(r.error || '命令失败');
      return r ? r.data : null;
    });
  }

  function mToast(msg, isErr) {
    var el = $('#toast');
    if (!el) return;
    el.textContent = msg;
    el.className = 'toast' + (isErr ? ' err' : '');
    el.hidden = false;
    clearTimeout(el._mt);
    el._mt = setTimeout(function () { el.hidden = true; }, 2800);
  }

  function mBytes(b) {
    var u = ['B', 'KB', 'MB', 'GB', 'TB'];
    var i = 0;
    var v = Number(b) || 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return (i === 0 ? v.toFixed(0) : v.toFixed(1)) + ' ' + u[i];
  }

  function mRate(bps) {
    return mBytes(bps) + '/s';
  }

  function mEsc(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  var SEV_LABEL = { notice: '留意', warn: '建议处理' };

  /* --------------------------- 渲染 --------------------------- */

  function renderSnapshot(s) {
    if (!s) return;
    mState.snap = s;
    var sm = s.sample || {};

    $('#monCpu').textContent = (sm.cpu_percent != null ? sm.cpu_percent.toFixed(0) : '—');
    $('#monMem').textContent = (sm.mem_percent != null ? sm.mem_percent.toFixed(0) + '%' : '—');
    $('#monDisk').textContent = (sm.disk_percent != null ? sm.disk_percent.toFixed(0) + '%' : '—');
    $('#monNet').textContent = mRate((sm.net_rx_bps || 0) + (sm.net_tx_bps || 0));
    $('#monProc').textContent = sm.process_count != null ? sm.process_count : '—';
    $('#monSelfMem').textContent = sm.self_working_set_mb != null
      ? sm.self_working_set_mb.toFixed(1) + ' MB' : '—';
    $('#monSelfCpu').textContent = sm.self_cpu_percent != null
      ? sm.self_cpu_percent.toFixed(2) + '%' : '—';

    var sub = s.running
      ? '正在后台采样（每 ' + Math.round((s.config.interval_ms || 5000) / 1000) + ' 秒一轮）。'
      : '常驻监控已关闭 —— 关闭后不占任何常驻资源。';
    $('#monSub').textContent = sub;

    $('#btnMonToggle').textContent = s.running ? '关闭常驻监控' : '开启常驻监控';

    renderAlerts(s.alerts || []);
    renderConfig(s.config || {});
  }

  function renderAlerts(list) {
    var ul = $('#monAlerts');
    if (!ul) return;
    if (!list.length) {
      ul.innerHTML = '<li class="junk-empty">目前没有告警，一切正常。</li>';
      return;
    }
    ul.innerHTML = list.map(function (a) {
      return '<li class="mon-alert ' + mEsc(a.level) + '">' +
        '<div class="mon-alert-head"><b>' + mEsc(a.title) + '</b>' +
        '<span class="mon-alert-kind">' + mEsc(SEV_LABEL[a.level] || a.level) + '</span></div>' +
        '<div class="mon-alert-detail">' + mEsc(a.detail) + '</div></li>';
    }).join('');
  }

  function renderConfig(cfg) {
    if (!cfg) return;
    if (document.activeElement !== $('#monCpuTh')) $('#monCpuTh').value = Math.round(cfg.cpu_threshold);
    if (document.activeElement !== $('#monMemTh')) $('#monMemTh').value = Math.round(cfg.mem_threshold);
    if (document.activeElement !== $('#monDiskTh')) $('#monDiskTh').value = Math.round(cfg.disk_threshold);
    if (document.activeElement !== $('#monInterval')) {
      $('#monInterval').value = Math.round((cfg.interval_ms || 5000) / 1000);
    }
  }

  /* --------------------------- 动作 --------------------------- */

  function loadSnapshot() {
    return mCall('monitor_snapshot').then(renderSnapshot).catch(function () {
      if (!IS_TAURI) {
        $('#monSub').textContent = '预览模式：没有真实系统数据。';
      }
    });
  }

  function toggleMonitor() {
    var s = mState.snap;
    var next = !(s && s.running);
    return mCall('monitor_toggle', { enabled: next })
      .then(function (snap) {
        renderSnapshot(snap);
        mToast(next ? '常驻监控已开启' : '常驻监控已关闭，常驻占用归零');
      })
      .catch(function (e) { mToast('操作失败：' + e, true); });
  }

  function clearAlerts() {
    return mCall('monitor_clear_alerts')
      .then(function () { return loadSnapshot(); })
      .then(function () { mToast('告警已清空'); })
      .catch(function (e) { mToast('操作失败：' + e, true); });
  }

  function saveThresholds() {
    var cfg = {
      enabled: !!(mState.snap && mState.snap.running),
      interval_ms: Math.max(1, Number($('#monInterval').value) || 5) * 1000,
      cpu_threshold: Number($('#monCpuTh').value) || 85,
      mem_threshold: Number($('#monMemTh').value) || 90,
      disk_threshold: Number($('#monDiskTh').value) || 95
    };
    return mCall('monitor_configure', { config: cfg })
      .then(function () { return loadSnapshot(); })
      .then(function () { mToast('阈值已保存'); })
      .catch(function (e) { mToast('保存失败：' + e, true); });
  }

  /* --------------------------- 悬浮球 --------------------------- */

  function loadFloatBall() {
    return mCall('floatball_state').then(function (st) {
      mState.fb = st;
      $('#fbSize').value = Math.round(st.size);
      $('#fbOpacity').value = Math.round(st.opacity * 100);
      $('#btnFbToggle').textContent = st.visible ? '隐藏悬浮球' : '显示悬浮球';
    }).catch(function () {});
  }

  function toggleFloatBall() {
    var st = mState.fb || { visible: false };
    return mCall('floatball_show', { show: !st.visible })
      .then(function (v) {
        if (mState.fb) mState.fb.visible = v;
        $('#btnFbToggle').textContent = v ? '隐藏悬浮球' : '显示悬浮球';
        mToast(v ? '悬浮球已显示' : '悬浮球已隐藏');
      })
      .catch(function (e) { mToast('操作失败：' + e, true); });
  }

  function saveFloatBall() {
    var st = mState.fb || {
      visible: false, x: 0, y: 0, size: 72, opacity: 0.92, collapsed: false
    };
    st.size = Number($('#fbSize').value) || 72;
    st.opacity = (Number($('#fbOpacity').value) || 92) / 100;
    return mCall('floatball_save', { state: st })
      .then(function (s) {
        mState.fb = s;
        $('#fbSize').value = Math.round(s.size);
        $('#fbOpacity').value = Math.round(s.opacity * 100);
        mToast('悬浮球设置已保存');
      })
      .catch(function (e) { mToast('保存失败：' + e, true); });
  }

  /* --------------------------- 生命周期 --------------------------- */

  function enterMonitorView() {
    loadSnapshot();
    loadFloatBall();
    if (!mState.timer) {
      mState.timer = setInterval(loadSnapshot, 3000);
    }
  }

  function bindMonitor() {
    var t = $('#btnMonToggle');
    if (t) t.addEventListener('click', toggleMonitor);
    var c = $('#btnMonClear');
    if (c) c.addEventListener('click', clearAlerts);
    var sv = $('#btnMonSave');
    if (sv) sv.addEventListener('click', saveThresholds);
    var fb = $('#btnFbToggle');
    if (fb) fb.addEventListener('click', toggleFloatBall);
    var fbs = $('#btnFbSave');
    if (fbs) fbs.addEventListener('click', saveFloatBall);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bindMonitor);
  } else {
    bindMonitor();
  }

  window.enterMonitorView = enterMonitorView;
  window.monitorLoadSnapshot = loadSnapshot;
})();
