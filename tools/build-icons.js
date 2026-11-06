/**
 * 从品牌 SVG 生成 Tauri 需要的图标资源。
 *
 * 产出（src-tauri/icons/）：
 *   32x32.png / 128x128.png / 128x128@2x.png / 256x256.png  —— Tauri 各平台图标
 *   icon.ico  —— Windows 可执行文件图标（内含多尺寸 PNG，Vista+ 原生支持）
 *
 * ICO 用「PNG 直嵌」格式：Windows Vista 以上完全支持，
 * 不需要额外的 BMP 编码器，体积也更小。
 *
 * 用法：node tools/build-icons.js
 */

const fs = require('fs');
const path = require('path');
const sharp = require('sharp');

const ROOT = path.resolve(__dirname, '..');
const SRC = path.join(ROOT, 'assets/logo/app-icon.svg');
const OUT = path.join(ROOT, 'src-tauri/icons');

// Windows 图标标准尺寸
const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];

async function png(size) {
  return sharp(SRC, { density: 384 })
    .resize(size, size, { fit: 'contain', background: { r: 0, g: 0, b: 0, alpha: 0 } })
    .png({ compressionLevel: 9 })
    .toBuffer();
}

/** 组装 ICO 容器（PNG 直嵌）。 */
function buildIco(entries) {
  const count = entries.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);      // reserved
  header.writeUInt16LE(1, 2);      // type = icon
  header.writeUInt16LE(count, 4);  // image count

  const dir = Buffer.alloc(16 * count);
  let offset = 6 + 16 * count;

  entries.forEach((e, i) => {
    const b = i * 16;
    // 0 表示 256
    dir.writeUInt8(e.size >= 256 ? 0 : e.size, b + 0);      // width
    dir.writeUInt8(e.size >= 256 ? 0 : e.size, b + 1);      // height
    dir.writeUInt8(0, b + 2);                                // palette
    dir.writeUInt8(0, b + 3);                                // reserved
    dir.writeUInt16LE(1, b + 4);                             // color planes
    dir.writeUInt16LE(32, b + 6);                            // bits per pixel
    dir.writeUInt32LE(e.data.length, b + 8);                 // size in bytes
    dir.writeUInt32LE(offset, b + 12);                       // offset
    offset += e.data.length;
  });

  return Buffer.concat([header, dir, ...entries.map((e) => e.data)]);
}

(async () => {
  fs.mkdirSync(OUT, { recursive: true });

  // 1. 标准 PNG
  const named = [
    [32, '32x32.png'],
    [128, '128x128.png'],
    [256, '128x128@2x.png'],
    [256, '256x256.png'],
  ];
  for (const [size, name] of named) {
    const buf = await png(size);
    fs.writeFileSync(path.join(OUT, name), buf);
    console.log(`  ${name.padEnd(18)} ${(buf.length / 1024).toFixed(1)} KB`);
  }

  // 2. ICO
  const entries = [];
  for (const size of ICO_SIZES) {
    entries.push({ size, data: await png(size) });
  }
  const ico = buildIco(entries);
  fs.writeFileSync(path.join(OUT, 'icon.ico'), ico);
  console.log(`  ${'icon.ico'.padEnd(18)} ${(ico.length / 1024).toFixed(1)} KB  (${ICO_SIZES.join('/')})`);

  // 3. 应用内使用的 PNG（Linux 打包 / 关于页）
  const appPng = await png(512);
  fs.writeFileSync(path.join(ROOT, 'assets/logo/app-icon.png'), appPng);
  console.log(`  app-icon.png       ${(appPng.length / 1024).toFixed(1)} KB`);

  console.log('\n完成 →', OUT);
})();
