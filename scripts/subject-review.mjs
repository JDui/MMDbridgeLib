import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = process.env.MBL_SUBJECT_REVIEW;
assert.ok(output, 'MBL_SUBJECT_REVIEW is required');
const require = createRequire(import.meta.url);
const { build } = require(join(root, 'apps/desktop/node_modules/esbuild'));
const { chromium } = require(process.env.MBL_PLAYWRIGHT_MODULE);
const manifest = JSON.parse(await fs.readFile(join(output, 'manifest.json'), 'utf8'));
assert.equal(manifest.synthetic, true); assert.equal(manifest.entries.length, 11);
await build({entryPoints: [join(root, 'apps/desktop/src/main.tsx')], bundle: true, format: 'iife',
  outfile: join(output, 'review.js'), minify: true, logLevel: 'silent'});
const [js, css, bridgeBase] = await Promise.all([
  fs.readFile(join(output, 'review.js'), 'utf8'), fs.readFile(join(output, 'review.css'), 'utf8'),
  fs.readFile(join(root, 'scripts/visual-review-fixture.js'), 'utf8'),
]);
const showcaseIds = ['normal', 'huge-rig', 'detached', 'flying', 'transparent', 'hair-wings', 'pmd-flying', 'dense-prop'];
const showcase = showcaseIds.map(id => manifest.entries.find(entry => entry.case === id));
const bridge = `${bridgeBase}\nconst entries=${JSON.stringify(showcase)};
const samples=entries.map(({asset})=>({...asset,rootId:'root-0',statuses:['Ready'],hasThumbnail:true,cardStatus:'CardValid'}));
const originalInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
 if(cmd==='asset_counts')return {all:samples.length,model:samples.length,motion:0,scene:0,byRoot:{'root-0':samples.length}};
 if(cmd==='assets_page')return {items:samples.filter(a=>(!args.assetType||a.assetType==='model')&&(!args.rootId||a.rootId===args.rootId)&&(!args.query||a.name.includes(args.query))),nextCursor:null};
 if(cmd==='asset_inspect')return samples.find(a=>a.id===args.assetId);
 if(cmd==='card_thumbnail'){
  const entry=entries.find(e=>e.asset.id===args.assetId);
  const variant=new URLSearchParams(location.search).get('variant')==='before'?'before':'after';
  return (await fetch('/'+variant+'/'+entry.case+'.webp')).arrayBuffer();
 }
 return originalInvoke(cmd,args);
};`;
const comparison = `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><style>
*{box-sizing:border-box}body{margin:0;padding:28px;background:#edf0f2;color:#263743;font:16px "Noto Sans CJK SC",sans-serif}
h1{margin:0 0 6px;font-size:25px;font-weight:600}p{margin:0 0 22px;color:#627482;font-size:14px}
.cases{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:20px}.pair{background:#fff;border-radius:14px;padding:16px}
h2{margin:0 0 12px;font-size:17px;font-weight:500}.images{display:grid;grid-template-columns:repeat(2,1fr);gap:12px}
figure{margin:0}figcaption{font-size:13px;color:#627482;margin-bottom:8px}img{display:block;width:100%;border-radius:9px}
</style><h1>角色缩略图取景对比</h1><p>相同合成模型 · 实际 Core 离屏渲染 · 软件 Vulkan · 1024 × 1024 WebP</p><div class="cases"></div>
<script>const entries=${JSON.stringify(manifest.entries)};
const ids=new URLSearchParams(location.search).get('cases').split(',');
for(const id of ids){const entry=entries.find(e=>e.case===id);const card=document.createElement('section');card.className='pair';
const title=document.createElement('h2');title.textContent=entry.asset.name;card.append(title);
const images=document.createElement('div');images.className='images';
for(const [variant,label]of[['before','原取景'],['after','主体取景']]){const figure=document.createElement('figure');
const caption=document.createElement('figcaption');caption.textContent=label;const image=document.createElement('img');
image.src='/'+variant+'/'+id+'.webp';figure.append(caption,image);images.append(figure);}card.append(images);document.querySelector('.cases').append(card);}</script>`;
const server = http.createServer(async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://127.0.0.1').pathname);
  if (path === '/comparison') {res.setHeader('content-type', 'text/html; charset=utf-8'); res.end(comparison); return;}
  const match = path.match(/^\/(before|after)\/([a-z-]+)\.webp$/);
  if (match) {res.setHeader('content-type', 'image/webp'); res.end(await fs.readFile(join(output, match[1], match[2] + '.webp'))); return;}
  if (path === '/review.js') {res.setHeader('content-type', 'application/javascript'); res.end(js); return;}
  if (path === '/review.css') {res.setHeader('content-type', 'text/css'); res.end(css); return;}
  res.setHeader('content-type', 'text/html; charset=utf-8');
  res.end(`<!doctype html><html lang="zh-CN"><meta charset="utf-8"><link rel="stylesheet" href="/review.css"><div id="root"></div><script>${bridge}</script><script src="/review.js"></script>`);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({headless: true});
const page = await browser.newPage({viewport: {width: 1480, height: 1100}, deviceScaleFactor: 1});
const errors = []; page.on('pageerror', error => errors.push(String(error)));
try {
  await page.goto(`${url}/comparison?cases=${manifest.entries.map(entry => entry.case).join(',')}`);
  await page.waitForFunction(() => document.images.length === 22 && [...document.images].every(image => image.complete && image.naturalWidth === 1024));
  const metrics = await page.evaluate(() => [...document.images].map(image => {
    const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1024;
    const context = canvas.getContext('2d'); context.drawImage(image, 0, 0);
    const pixels = context.getImageData(0, 0, 1024, 1024).data;
    const background = pixels.slice(0, 3); let count = 0; let minimum = [1024, 1024]; let maximum = [-1, -1];
    for (let i = 0; i < pixels.length; i += 4) {
      if (Math.abs(pixels[i] - background[0]) + Math.abs(pixels[i + 1] - background[1]) + Math.abs(pixels[i + 2] - background[2]) <= 30) continue;
      const x = i / 4 % 1024; const y = Math.floor(i / 4 / 1024); count++;
      minimum = [Math.min(minimum[0], x), Math.min(minimum[1], y)]; maximum = [Math.max(maximum[0], x), Math.max(maximum[1], y)];
    }
    return {path: new URL(image.src).pathname, width: image.naturalWidth, height: image.naturalHeight,
      foregroundPixels: count, minimum, maximum};
  }));
  const metric = (variant, id) => metrics.find(value => value.path === `/${variant}/${id}.webp`);
  const normal = metric('after', 'normal').foregroundPixels;
  assert.ok(normal > 80000, 'normal subject is visible');
  for (const id of ['detached', 'flying', 'transparent', 'pmd-flying', 'dense-prop', 'tiny']) {
    assert.ok(metric('after', id).foregroundPixels > normal * 0.85, `${id}: subject occupies the image`);
    assert.ok(metric('after', id).foregroundPixels > metric('before', id).foregroundPixels * 2, `${id}: improves over full bounds`);
  }
  for (const id of ['normal', 'huge-rig', 'unreferenced', 'large']) {
    assert.ok(Math.abs(metric('after', id).foregroundPixels / normal - 1) < 0.01, `${id}: framing stays consistent`);
  }
  const wings = metric('after', 'hair-wings');
  assert.ok(wings.minimum[0] >= 25 && wings.maximum[0] <= 999 && wings.minimum[1] >= 25 && wings.maximum[1] <= 999,
    'retained long hair, skirt and wings have a margin');
  for (const id of ['normal', 'huge-rig', 'unreferenced', 'detached', 'dense-prop']) {
    assert.deepEqual(await fs.readFile(join(output, 'after', id + '.webp')),
      await fs.readFile(join(output, 'after', 'normal.webp')), `${id}: complete original subject is retained`);
  }
  await page.goto(`${url}/comparison?cases=detached,flying,pmd-flying,dense-prop`);
  await page.waitForFunction(() => [...document.images].every(image => image.complete && image.naturalWidth === 1024));
  await page.screenshot({path: join(output, 'MBL_Subject_Stage1_Outliers.jpg'), fullPage: true, type: 'jpeg', quality: 92, animations: 'disabled'});
  await page.goto(`${url}/comparison?cases=huge-rig,hair-wings,transparent,tiny`);
  await page.waitForFunction(() => [...document.images].every(image => image.complete && image.naturalWidth === 1024));
  await page.screenshot({path: join(output, 'MBL_Subject_Stage2_Preservation.jpg'), fullPage: true, type: 'jpeg', quality: 92, animations: 'disabled'});
  const imageBoxes = [];
  for (const [variant, file] of [['before', 'MBL_Subject_Stage3_Library_Before.jpg'], ['after', 'MBL_Subject_Stage4_Library_After.jpg']]) {
    await page.goto(`${url}/?scheme=dark&variant=${variant}`);
    await page.waitForFunction(() => document.querySelectorAll('.asset-card img').length === 8 && [...document.querySelectorAll('.asset-card img')].every(image => image.complete && image.naturalWidth === 1024));
    const boxes = await page.locator('.asset-card img').evaluateAll(images => images.map(image => {
      const imageRect = image.getBoundingClientRect(); const slot = image.parentElement.getBoundingClientRect();
      return {name: image.alt, width: imageRect.width, height: imageRect.height, slotWidth: slot.width, slotHeight: slot.height};
    }));
    assert.ok(boxes.every(box => Math.abs(box.width - box.slotWidth) < 1 && Math.abs(box.height - box.slotHeight) < 1),
      'thumbnail element fits the slot so contain preserves the full square image');
    imageBoxes.push({variant, boxes});
    await page.screenshot({path: join(output, file), type: 'jpeg', quality: 92, animations: 'disabled'});
  }
  assert.deepEqual(errors, []);
  await fs.writeFile(join(output, 'verification.json'), JSON.stringify({sourceCommit: manifest.sourceCommit,
    baselineCommit: manifest.baselineCommit, synthetic: true, headless: true, metrics, subjectChecksPassed: true,
    uiImageCount: 8, imageBoxes, pageErrors: errors}, null, 2));
  console.log(JSON.stringify({synthetic: true, comparisons: 11, subjectChecksPassed: true, screenshots: 4, pageErrors: errors}));
} catch (error) {
  await page.screenshot({path: join(output, 'failure.jpg'), fullPage: true});
  throw error;
} finally {await browser.close(); await new Promise(resolve => server.close(resolve));}
