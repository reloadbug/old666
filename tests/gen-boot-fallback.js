// 开机性能日志通道（Microsoft-Windows-Diagnostics-Performance/Operational）
// 在这台机器上被清空后一直没再写入事件 100，真机导出的 boot.json 会是空壳
// （available=false / records=[]），前端测试就会卡在「等分段条」。
//
// 这个脚本只在真机导出为空时，生成一份**结构完全一致**的合成 fixture，
// 目的只是让前端的渲染路径仍然被覆盖到（分段条 / 图例 / 趋势图 / 归因列表）。
// 合成数据统一带 fallback 标记，不会被当成真实测量结果。
//
// 用法：node tests/gen-boot-fallback.js
const fs = require('fs');
const path = require('path');

const OUT = path.join(__dirname, '_out', 'fixtures', 'boot.json');
if (!fs.existsSync(path.dirname(OUT))) {
  console.error('先建目录：', path.dirname(OUT));
  process.exit(1);
}

let cur = null;
try {
  cur = JSON.parse(fs.readFileSync(OUT, 'utf8').replace(/^\uFEFF/, ''));
} catch {
  cur = null;
}

if (cur && cur.available && Array.isArray(cur.records) && cur.records.length) {
  console.log('真机数据可用，不需要 fallback（records=' + cur.records.length + '）');
  process.exit(0);
}

const PHASES = [
  { key: 'firmware', label: '固件', ms: 2100, kind: 'main', actionable: false },
  { key: 'kernel', label: '内核', ms: 3100, kind: 'main', actionable: false },
  { key: 'drivers', label: '驱动', ms: 4200, kind: 'main', actionable: true },
  { key: 'devices', label: '设备', ms: 1500, kind: 'main', actionable: true },
  { key: 'services', label: '服务', ms: 3800, kind: 'main', actionable: true },
  { key: 'smss', label: '登录', ms: 900, kind: 'main', actionable: false },
  { key: 'profile', label: '用户配置', ms: 1700, kind: 'main', actionable: false },
  { key: 'explorer', label: '桌面', ms: 1400, kind: 'main', actionable: false },
  { key: 'postboot', label: '后台收尾', ms: 2600, kind: 'post', actionable: true },
];

const CULPRITS = [
  { name: 'WorkBuddyAI.exe', friendly_name: 'WorkBuddy AI', kind: 'app', total_ms: 54100, degradation_ms: 54100, path: 'C:\\Program Files\\WorkBuddy\\WorkBuddyAI.exe', company: 'WorkBuddy' },
  { name: 'WorkBuddy.exe', friendly_name: 'WorkBuddy', kind: 'app', total_ms: 20400, degradation_ms: 20400, path: 'C:\\Program Files\\WorkBuddy\\WorkBuddy.exe', company: 'WorkBuddy' },
  { name: 'nvngx_update.exe', friendly_name: 'NVIDIA 显卡更新', kind: 'app', total_ms: 900, degradation_ms: 326, path: 'C:\\Program Files\\NVIDIA Corporation\\nvngx_update.exe', company: 'NVIDIA' },
];

function rec(total, degraded) {
  const phases = PHASES.map((p) => ({ ...p }));
  return {
    boot_ms: total,
    main_path_ms: total - 2600,
    post_boot_ms: 2600,
    start_time: '2026-11-20T08:12:03.000Z',
    end_time: '2026-11-20T08:13:03.000Z',
    degraded,
    startup_apps: 24,
    phases,
    culprits: degraded ? CULPRITS.map((c) => ({ ...c })) : [],
  };
}

const records = [
  rec(50100, true),
  rec(43200, false),
  rec(38900, false),
  rec(61100, true),
];

const out = {
  records,
  avg_ms: Math.round(records.reduce((a, r) => a + r.boot_ms, 0) / records.length),
  best_ms: Math.min(...records.map((r) => r.boot_ms)),
  worst_ms: Math.max(...records.map((r) => r.boot_ms)),
  available: true,
  note: '合成数据（fallback）：真机开机性能日志通道为空，仅用于覆盖前端渲染路径，不是真实测量结果。',
};

fs.writeFileSync(OUT, JSON.stringify(out, null, 2), 'utf8');
console.log('已生成 fallback boot.json：records=' + out.records.length);
