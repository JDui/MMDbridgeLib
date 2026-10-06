import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = process.env.MBL_MODEL_REVIEW;
assert.ok(output, 'MBL_MODEL_REVIEW is required');
const require = createRequire(import.meta.url);
const { build } = require(join(root, 'apps/desktop/node_modules/esbuild'));
const { chromium } = require(process.env.MBL_PLAYWRIGHT_MODULE);
const manifest = JSON.parse(await fs.readFile(join(output,'manifest.json'),'utf8'));
assert.equal(manifest.synthetic,true); assert.equal(manifest.entries.length,7);
await build({entryPoints:[join(root,'apps/desktop/src/main.tsx')], bundle:true,format:'iife',
  outfile:join(output,'review.js'),minify:true,logLevel:'silent'});
const [js,css,base] = await Promise.all([
  fs.readFile(join(output,'review.js'),'utf8'),fs.readFile(join(output,'review.css'),'utf8'),
  fs.readFile(join(root,'scripts/visual-review-fixture.js'),'utf8'),
]);
const bridge = `${base}\nconst modelEntries=${JSON.stringify(manifest.entries)};
const modelAssets=modelEntries.map(({asset})=>({...asset,rootId:asset.assetType==='motion'?'root-1':'root-0',statuses:['Ready'],hasThumbnail:true,cardStatus:'CardValid'}));
const originalInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
 if(cmd==='asset_counts')return {all:7,model:5,motion:2,scene:0,byRoot:{'root-0':5,'root-1':2}};
 if(cmd==='assets_page')return {items:modelAssets.filter(a=>(!args.assetType||a.assetType===args.assetType)&&(!args.rootId||a.rootId===args.rootId)&&(!args.query||a.name.toLowerCase().includes(args.query.toLowerCase()))),nextCursor:null};
 if(cmd==='asset_inspect')return modelAssets.find(a=>a.id===args.assetId);
 if(cmd==='card_thumbnail')return (await fetch('/'+args.assetId+'.webp')).arrayBuffer();
 if(cmd==='model_preview'){
  if(new URLSearchParams(location.search).has('corrupt'))throw new Error('模型数据截断或区段长度无效');
  return (await fetch('/'+args.assetId+'.bin')).arrayBuffer();
 }
 if(cmd==='model_preview_texture_file'){
  const png=new Uint8Array(await (await fetch('/preview-texture.png')).arrayBuffer());
  const bytes=new Uint8Array(png.length+1);bytes.set(png,1);return bytes.buffer;
 }
 return originalInvoke(cmd,args);
};`;
const html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>MBL</title><style id="library-style-nonce"></style><style>${css}</style></head><body><div id="root"></div><script>${bridge}</script><script>${js.replaceAll('</script','<\\/script')}</script></body></html>`;
const files = new Map();
for(const entry of manifest.entries) {
  files.set(`/${entry.asset.id}.webp`,{bytes:await fs.readFile(join(output,`${entry.asset.id}.webp`)),type:'image/webp'});
  if(entry.asset.assetType==='model')files.set(`/${entry.asset.id}.bin`,{bytes:await fs.readFile(join(output,`${entry.asset.id}.bin`)),type:'application/octet-stream'});
}
files.set('/preview-texture.png',{bytes:await fs.readFile(join(output,'preview-texture.png')),type:'image/png'});
const server = http.createServer((request,response)=>{
  const asset=files.get(new URL(request.url,'http://localhost').pathname);
  response.writeHead(200,{'Content-Type':asset?.type??'text/html; charset=utf-8'});response.end(asset?.bytes??html);
});
await new Promise((ready,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',ready);});
let browser;
try {
  browser=await chromium.launch({headless:true,args:['--use-angle=swiftshader','--enable-unsafe-swiftshader']});
  const page=await browser.newPage({viewport:{width:1480,height:960},locale:'zh-CN',reducedMotion:'reduce'});
  page.setDefaultTimeout(20000);const errors=[];page.on('pageerror',error=>errors.push(error.message));
  const url=`http://127.0.0.1:${server.address().port}`;
  await page.goto(`${url}/?scheme=dark`);
  await page.locator('.asset-card').first().waitFor();
  await page.waitForFunction(()=>[...document.querySelectorAll('.asset-card img')].length===7&&[...document.querySelectorAll('.asset-card img')].every(image=>image.complete&&image.naturalWidth===1024));
  await page.evaluate(()=>document.fonts.ready);
  await page.screenshot({path:join(output,'MBL_Model_Stage1_Library_Dark.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  const pmd=manifest.entries.find(entry=>entry.asset.assetType==='model'&&entry.asset.metadata.file_type==='pmd');assert.ok(pmd);
  await page.getByRole('button',{name:new RegExp(pmd.asset.name)}).first().dblclick();
  const viewer=page.getByRole('dialog',{name:`${pmd.asset.name} 3D 查看器`});
  await viewer.getByText('正在由 Rust Core 解析 3D 网格…').waitFor({state:'hidden'});
  await viewer.getByText('已加载贴图 1 / 1',{exact:true}).waitFor();
  await viewer.getByRole('button',{name:'权重类型',exact:true}).click();
  assert.equal(await viewer.locator('canvas').count(),1);
  await page.screenshot({path:join(output,'MBL_Model_Stage2_PMD_Weights.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await viewer.getByRole('button',{name:'关闭 3D 预览',exact:true}).click();
  await page.goto(`${url}/?scheme=light`);
  await page.waitForFunction(()=>[...document.querySelectorAll('.asset-card img')].length===7&&[...document.querySelectorAll('.asset-card img')].every(image=>image.complete&&image.naturalWidth===1024));
  await page.screenshot({path:join(output,'MBL_Model_Stage3_Library_Light.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.goto(`${url}/?scheme=dark&corrupt=1`);
  await page.getByRole('button',{name:new RegExp(pmd.asset.name)}).first().dblclick();
  await page.locator('.model-viewer-error').filter({hasText:'模型数据截断或区段长度无效'}).waitFor();
  await page.screenshot({path:join(output,'MBL_Model_Stage4_Load_Error.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.getByRole('button',{name:'关闭 3D 预览',exact:true}).click();
  assert.equal(await page.locator('.asset-card').count(),7);
  assert.deepEqual(errors,[]);
  const decoded=await page.evaluate(async()=>{
    const images=[...document.querySelectorAll('.asset-card img')];
    await Promise.all(images.map(image=>image.decode()));
    return images.map(image=>{
      const canvas=document.createElement('canvas');canvas.width=1024;canvas.height=1024;
      const context=canvas.getContext('2d');context.drawImage(image,0,0);
      const data=context.getImageData(0,0,1024,1024).data;
      let changed=0;for(let i=4;i<data.length;i+=4)if(Math.abs(data[i]-data[0])+Math.abs(data[i+1]-data[1])+Math.abs(data[i+2]-data[2])>20)changed++;
      return {width:image.naturalWidth,height:image.naturalHeight,nonBackgroundPixels:changed};
    });
  });
  assert.equal(decoded.length,7);assert.ok(decoded.every(image=>image.nonBackgroundPixels>10000));
  await fs.writeFile(join(output,'ui-verification.json'),JSON.stringify({sourceCommit:process.env.GITHUB_SHA,synthetic:true,headless:true,
    decoded,pmdViewerLoaded:true,loadErrorClosed:true,pageErrors:errors},null,2));
  console.log('Seven Core-generated previews decoded; PMD weights loaded; load-error recovery verified.');
} finally {await browser?.close();await new Promise(resolve=>server.close(resolve));}
