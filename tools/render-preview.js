const sharp = require('sharp');
const fs = require('fs');

const S = Number(process.env.S || 340);
const BG = process.env.BG || '#ffffff';
const OUT = process.env.OUT || 'assets/logo/_compare.png';

(async () => {
  const files = process.argv.slice(2);
  const tiles = [];
  for (const f of files) {
    const buf = await sharp(fs.readFileSync(f), { density: 400 })
      .resize(S, S, { fit: 'contain', background: { r: 255, g: 255, b: 255, alpha: 0 } })
      .flatten({ background: BG })
      .png()
      .toBuffer();
    tiles.push(buf);
    console.log('rendered', f);
  }
  const W = S * tiles.length;
  await sharp({ create: { width: W, height: S, channels: 4, background: BG } })
    .composite(tiles.map((b, i) => ({ input: b, left: i * S, top: 0 })))
    .png()
    .toFile(OUT);
  console.log('OK ->', OUT, W, 'x', S, 'bg', BG);
})().catch(e => { console.error('ERR', e.message); process.exit(1); });
