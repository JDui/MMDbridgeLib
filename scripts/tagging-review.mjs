import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
assert.ok(process.env.MBL_MODEL_REVIEW);
const output = join(process.env.MBL_MODEL_REVIEW, 'tagging');
const require = createRequire(import.meta.url);
const { build } = require(join(root, 'apps/desktop/node_modules/esbuild'));
const { chromium } = require(process.env.MBL_PLAYWRIGHT_MODULE);
const manifest = JSON.parse(await fs.readFile(join(output, 'manifest.json'), 'utf8'));
assert.equal(manifest.synthetic, true);
await build({entryPoints:[join(root,'apps/desktop/src/main.tsx')],bundle:true,format:'iife',
  outfile:join(output,'review.js'),minify:true,logLevel:'silent'});
const [js, css, base] = await Promise.all([
  fs.readFile(join(output,'review.js'),'utf8'),fs.readFile(join(output,'review.css'),'utf8'),
  fs.readFile(join(root,'scripts/visual-review-fixture.js'),'utf8'),
]);
const bridge = `${base}
const tagEntries=${JSON.stringify(manifest.entries)};
const tagBefore=new URLSearchParams(location.search).has('before');
let tagSettings={technical:true,colors:true};
window.__tagSaves=[];window.__tagFail=false;
const originalInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
 if(cmd==='asset_counts')return {all:2,model:2,motion:0,scene:0,byRoot:{'root-0':2}};
 if(cmd==='assets_page')return {items:tagEntries.map(({asset})=>({...asset,rootId:'root-0',hasThumbnail:!tagBefore,cardStatus:tagBefore?'CardMissing':'CardValid'})),nextCursor:null};
 if(cmd==='asset_inspect')return {...tagEntries.find(({asset})=>asset.id===args.assetId).asset,rootId:'root-0',hasThumbnail:!tagBefore,cardStatus:tagBefore?'CardMissing':'CardValid'};
 if(cmd==='asset_tags'){const entry=tagEntries.find(({asset})=>asset.id===args.assetId);return tagBefore?entry.beforeTags:entry.afterTags;}
 if(cmd==='tags_list')return [...new Set(tagEntries.flatMap(entry=>(tagBefore?entry.beforeTags:entry.afterTags).map(tag=>tag.name)))];
 if(cmd==='card_thumbnail')return (await fetch('/'+args.assetId+'.webp')).arrayBuffer();
 if(cmd==='auto_tag_settings_get')return tagSettings;
 if(cmd==='auto_tag_settings_set'){
  if(window.__tagFail)throw new Error('标签设置无法保存');
  tagSettings={...args.settings};window.__tagSaves.push(tagSettings);return tagSettings;
 }
 return originalInvoke(cmd,args);
};`;
const html=`<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>MBL</title><style id="library-style-nonce"></style><style>${css}</style></head><body><div id="root"></div><script>${bridge}</script><script>${js.replaceAll('</script','<\\/script')}</script></body></html>`;
const files=new Map();
for(const {asset} of manifest.entries)files.set('/'+asset.id+'.webp',await fs.readFile(join(output,asset.id+'.webp')));
const server=http.createServer((request,response)=>{
  const image=files.get(new URL(request.url,'http://localhost').pathname);
  response.writeHead(200,{'Content-Type':image?'image/webp':'text/html; charset=utf-8'});response.end(image??html);
});
await new Promise((ready,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',ready);});
let browser;
try {
  browser=await chromium.launch({headless:true});
  const page=await browser.newPage({viewport:{width:1480,height:960},locale:'zh-CN',reducedMotion:'reduce'});
  page.setDefaultTimeout(20000);const errors=[];page.on('pageerror',error=>errors.push(error.message));
  const url=`http://127.0.0.1:${server.address().port}`;
  const select=async()=>{
    await page.locator('.asset-card').first().waitFor();
    await page.getByRole('button',{name:/蓝色材质测试/}).first().click();
    const showDetails=page.getByRole('button',{name:'显示资产详情',exact:true});
    if(await showDetails.count())await showDetails.click();
    await page.locator('.tag-chip-name').filter({hasText:'技术:含SDEF'}).waitFor();
    await page.evaluate(()=>document.fonts.ready);
  };
  await page.goto(`${url}/?scheme=dark&before=1`);await select();
  assert.equal(await page.locator('.tag-chip-name').filter({hasText:'整体色:'}).count(),0);
  assert.equal(await page.locator('.inspector-preview img').count(),0);
  await page.locator('.tags-section').scrollIntoViewIfNeeded();
  await page.screenshot({path:join(output,'MBL_Tags_Stage1_Scan.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.goto(`${url}/?scheme=dark`);await select();
  await page.waitForFunction(()=>[...document.querySelectorAll('.asset-card img')].length===2&&[...document.querySelectorAll('.asset-card img')].every(image=>image.complete&&image.naturalWidth===1024));
  await page.locator('.tag-chip-name').filter({hasText:'整体色:蓝色'}).waitFor();
  await page.locator('.tags-section').scrollIntoViewIfNeeded();
  await page.screenshot({path:join(output,'MBL_Tags_Stage2_Colours.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.getByRole('button',{name:'设置',exact:true}).click();
  const technical=page.getByRole('checkbox',{name:'扫描时生成技术标签',exact:true});
  const colors=page.getByRole('checkbox',{name:'从角色缩略图提取整体色',exact:true});
  assert.equal(await technical.isChecked(),true);assert.equal(await colors.isChecked(),true);
  await page.screenshot({path:join(output,'MBL_Tags_Stage3_Settings.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await colors.uncheck();await page.getByRole('button',{name:'保存标签设置',exact:true}).click();
  await page.waitForFunction(()=>window.__tagSaves.length===1);
  assert.deepEqual(await page.evaluate(()=>window.__tagSaves[0]),{technical:true,colors:false});
  await page.getByRole('button',{name:'关闭窗口',exact:true}).click();
  await page.getByRole('button',{name:'设置',exact:true}).click();
  assert.equal(await colors.isChecked(),false);
  await page.evaluate(()=>{window.__tagFail=true;});
  await technical.uncheck();await page.getByRole('button',{name:'保存标签设置',exact:true}).click();
  await page.getByRole('alert').filter({hasText:'标签设置无法保存'}).first().waitFor();
  assert.equal(await page.evaluate(()=>window.__tagSaves.length),1);
  await page.getByRole('button',{name:'关闭窗口',exact:true}).click();
  await page.getByRole('button',{name:'设置',exact:true}).click();
  assert.equal(await technical.isChecked(),true);
  assert.deepEqual(errors,[]);
  await fs.writeFile(join(output,'ui-verification.json'),JSON.stringify({synthetic:true,source:'Core tagging_probe fixtures',checks:{scanStageTags:true,thumbnailStageColor:true,controlsPersist:true,failedSavePreservesStoredValues:true},pageErrors:errors},null,2));
} finally {
  await browser?.close();await new Promise(done=>server.close(done));
}
