import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Isolated presentation tests. Live Core/CLI behavior is verified by agentlink_probe.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
assert.ok(process.env.MBL_MODEL_REVIEW);
const output = join(process.env.MBL_MODEL_REVIEW, 'agentlink');
const require = createRequire(import.meta.url);
const { build } = require(join(root, 'apps/desktop/node_modules/esbuild'));
const { chromium } = require(process.env.MBL_PLAYWRIGHT_MODULE);
const manifest = JSON.parse(await fs.readFile(join(output, 'manifest.json'), 'utf8'));
assert.equal(manifest.synthetic, true);
await build({entryPoints:[join(root,'apps/desktop/src/main.tsx')],bundle:true,format:'iife',
  outfile:join(output,'review.js'),minify:true,logLevel:'silent'});
const [js, css, base, preview] = await Promise.all([
  fs.readFile(join(output,'review.js'),'utf8'),fs.readFile(join(output,'review.css'),'utf8'),
  fs.readFile(join(root,'scripts/visual-review-fixture.js'),'utf8'),fs.readFile(join(output,'preview.webp')),
]);
const bridge = `${base}
const agentReview=${JSON.stringify(manifest)};
const stages=agentReview.snapshots;
window.__agentState=structuredClone(stages[new URLSearchParams(location.search).get('stage')||'waiting']);
window.__agentCalls=[];window.__agentFailScope=false;window.__agentFailOpen=false;window.__assetReads=0;
const originalInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
 if(cmd==='roots_list')return agentReview.roots;
 if(cmd==='asset_counts')return {all:1,model:1,motion:0,scene:0,byRoot:{[agentReview.roots[0].id]:1}};
 if(cmd==='assets_page'){window.__assetReads++;return {items:[agentReview.asset],nextCursor:null};}
 if(cmd==='asset_inspect')return agentReview.asset;
 if(cmd==='asset_tags')return agentReview.tags;
 if(cmd==='tags_list')return agentReview.tags.map(tag=>tag.name);
 if(cmd==='card_thumbnail')return (await fetch('/preview.webp')).arrayBuffer();
 if(cmd==='agentlink_open'){
  if(window.__agentFailOpen)throw new Error('隔离测试：会话暂时无法读取');
  window.__agentCalls.push(cmd);return structuredClone(window.__agentState);
 }
 if(cmd==='agentlink_scope_set'){
  window.__agentCalls.push(cmd);
  if(window.__agentFailScope)throw new Error('隔离测试：范围无法保存');
  if(window.__agentState.status==='active')throw new Error('接管期间不能更改范围');
  const template=args.scope.rootId?stages.scopedWaiting:args.scope.assetType==='model'?stages.modelWaiting:stages.waiting;
  window.__agentState={...window.__agentState,scope:args.scope,prompt:template.prompt,revision:window.__agentState.revision+1};
  return structuredClone(window.__agentState);
 }
 if(cmd==='agentlink_cancel'){
  window.__agentCalls.push(cmd);window.__agentState={...window.__agentState,status:'cancelled',percent:null,revision:window.__agentState.revision+1};
  return structuredClone(window.__agentState);
 }
 if(cmd==='agentlink_new_session'){
  window.__agentCalls.push(cmd);window.__agentState={...window.__agentState,status:'waiting',agentName:'',percent:null,sessionId:stages.renewed.sessionId,revision:window.__agentState.revision+1};
  return structuredClone(window.__agentState);
 }
 return originalInvoke(cmd,args);
};`;
const html=`<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>MBL</title><style id="library-style-nonce"></style><style>${css}</style></head><body><div id="root"></div><script>${bridge.replaceAll('</script','<\\/script')}</script><script>${js.replaceAll('</script','<\\/script')}</script></body></html>`;
const server=http.createServer((request,response)=>{
  const image=new URL(request.url,'http://localhost').pathname==='/preview.webp';
  response.writeHead(200,{'Content-Type':image?'image/webp':'text/html; charset=utf-8'});response.end(image?preview:html);
});
await new Promise((ready,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',ready);});
let browser;
try {
  browser=await chromium.launch({headless:true});
  const context=await browser.newContext({viewport:{width:1480,height:960},locale:'zh-CN',reducedMotion:'reduce',permissions:['clipboard-read','clipboard-write']});
  const page=await context.newPage();page.setDefaultTimeout(20000);
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  const url=`http://127.0.0.1:${server.address().port}`;
  const tab=page.getByRole('button',{name:'AgentLink',exact:true});
  const prompt=page.getByRole('textbox',{name:'AgentLink Prompt',exact:true});
  const type=page.getByLabel('AgentLink 资产类型',{exact:true});
  const directory=page.getByLabel('AgentLink 资产目录',{exact:true});
  const open=async(stage,scheme)=>{
    await page.goto(url+'/?stage='+stage+'&scheme='+scheme);
    await page.locator('.asset-card').first().waitFor();await tab.click();
    await page.waitForFunction(()=>document.querySelector('.agentlink-prompt')?.value.includes('执行要求'));
    await page.evaluate(()=>document.fonts.ready);
  };
  await open('waiting','light');
  const boxes=await Promise.all(['.prompt-panel','.log-panel'].map(selector=>page.locator(selector).boundingBox()));
  assert.ok(boxes[0]&&boxes[1]&&boxes[0].x+boxes[0].width<boxes[1].x,'Prompt and Log must be side by side');
  assert.equal(await page.locator('.library-page').isVisible(),false);
  await page.screenshot({path:join(output,'MBL_AgentLink_Stage1_Prompt_Light.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  const generated=await prompt.inputValue();
  await page.locator('.prompt-panel').getByRole('button',{name:'复制',exact:true}).click();
  assert.equal(await page.evaluate(()=>navigator.clipboard.readText()),generated);
  await prompt.fill(generated+'\n优先检查模型预览。');
  await page.locator('.library-home').click();
  assert.equal(await page.locator('.library-page').isVisible(),true);await tab.click();
  assert.equal(await prompt.inputValue(),generated+'\n优先检查模型预览。');
  await type.selectOption('model');
  await page.getByRole('status').filter({hasText:'范围已变化'}).waitFor();
  assert.equal(await page.locator('.prompt-panel').getByRole('button',{name:'复制',exact:true}).isDisabled(),true);
  await page.getByRole('button',{name:'重新生成',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('.agentlink-prompt-warning'));
  assert.equal(await prompt.inputValue(),manifest.snapshots.modelWaiting.prompt);
  await page.evaluate(()=>{window.__agentFailScope=true;});
  await type.selectOption('scene');
  await page.getByRole('alert').filter({hasText:'范围无法保存'}).waitFor();
  assert.equal(await type.inputValue(),'model');
  await page.evaluate(()=>{window.__agentFailScope=false;});
  await directory.selectOption(manifest.roots[0].id);
  await page.waitForFunction(id=>document.querySelector('[aria-label="AgentLink 资产目录"]')?.value===id,manifest.roots[0].id);
  const waitingReads=await page.evaluate(()=>window.__assetReads);
  await page.locator('.library-home').click();
  await page.evaluate(active=>{window.__agentState=structuredClone(active);},manifest.snapshots.active);
  await page.waitForFunction(()=>document.querySelector('.agentlink-status')?.textContent.includes('正在接管'));
  await page.evaluate(finished=>{window.__agentState=structuredClone(finished);},manifest.snapshots.finished);
  await page.waitForFunction(before=>window.__assetReads>before,waitingReads);
  await tab.click();await page.locator('.agentlink-status').filter({hasText:'已完成 · Codex'}).waitFor();
  await open('active','dark');
  await page.locator('.agentlink-status').filter({hasText:'正在接管 · Codex'}).waitFor();
  assert.equal(await type.isDisabled(),true);assert.equal(await directory.isDisabled(),true);
  assert.equal(await page.locator('.agentlink-progress span').textContent(),'60%');
  await page.screenshot({path:join(output,'MBL_AgentLink_Stage2_Log_Dark.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  const reads=await page.evaluate(()=>window.__assetReads);
  await page.locator('.library-home').click();
  await page.evaluate(finished=>{window.__agentState=structuredClone(finished);},manifest.snapshots.finished);
  await page.waitForFunction(before=>window.__assetReads>before,reads);
  await tab.click();await page.locator('.agentlink-status').filter({hasText:'已完成 · Codex'}).waitFor();
  assert.equal(await page.locator('.agentlink-progress span').textContent(),'100%');
  await page.screenshot({path:join(output,'MBL_AgentLink_Stage3_Finished.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.getByRole('button',{name:'新会话',exact:true}).click();
  await page.locator('.agentlink-status').filter({hasText:'等待连接'}).waitFor();
  await page.evaluate(()=>{window.__agentState={...window.__agentState,status:'active',agentName:'Codex',percent:25,revision:window.__agentState.revision+1};});
  await page.locator('.agentlink-status').filter({hasText:'正在接管 · Codex'}).waitFor();
  await page.getByRole('button',{name:'取消接管',exact:true}).click();
  await page.locator('.agentlink-status').filter({hasText:'已取消'}).waitFor();
  await page.getByRole('button',{name:'新会话',exact:true}).click();
  await page.locator('.agentlink-status').filter({hasText:'等待连接'}).waitFor();
  await page.evaluate(()=>{window.__agentFailOpen=true;});
  await page.getByRole('alert').filter({hasText:'会话暂时无法读取'}).waitFor();
  await page.evaluate(()=>{window.__agentFailOpen=false;});
  await page.getByRole('alert').filter({hasText:'会话暂时无法读取'}).waitFor({state:'hidden'});
  const follow=page.getByRole('checkbox',{name:'跟随最新日志',exact:true});
  await follow.uncheck();
  await page.evaluate(()=>{
    const events=Array.from({length:80},(_,i)=>({id:'review-'+i,time:new Date().toISOString(),operation:'agent-log',phase:'note',message:i===79?'<script>window.__unsafe=true</script>':'隔离测试记录 '+i,percent:null,details:{}}));
    window.__agentState={...window.__agentState,events,revision:window.__agentState.revision+1};
  });
  await page.locator('.agentlink-event').nth(79).waitFor();
  assert.equal(await page.evaluate(()=>window.__unsafe),undefined);
  await page.locator('.agentlink-log').evaluate(element=>{element.scrollTop=120;});
  await page.evaluate(()=>{
    window.__agentState.events.push({id:'last',time:new Date().toISOString(),operation:'agent-log',phase:'note',message:'追加记录',percent:null,details:{}});window.__agentState.revision++;
  });
  await page.locator('.agentlink-event').nth(80).waitFor();
  assert.equal(await page.locator('.agentlink-log').evaluate(element=>element.scrollTop),120);
  await follow.check();
  await page.waitForFunction(()=>{const log=document.querySelector('.agentlink-log');return log.scrollTop+log.clientHeight>=log.scrollHeight-2;});
  await page.setViewportSize({width:1080,height:760});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  const narrow=await Promise.all(['.prompt-panel','.log-panel'].map(selector=>page.locator(selector).boundingBox()));
  assert.ok(narrow.every(box=>box&&box.width>250));
  assert.deepEqual(errors,[]);
  await fs.writeFile(join(output,'ui-verification.json'),JSON.stringify({synthetic:true,source:'Actual app frontend, Core snapshots, isolated Tauri presentation bridge',checks:{sideBySidePanels:true,promptClipboard:true,editedPromptPersists:true,scopeRegeneration:true,failedScopePreservesValues:true,activeScopeLocked:true,backgroundConnectionAndCompletionRefresh:true,pollingAfterNewSessionAndCancel:true,connectionRetry:true,logTextEscaped:true,manualScrollPreserved:true,followLatest:true,compactViewport:true},pageErrors:errors},null,2));
} finally {
  await browser?.close();await new Promise(done=>server.close(done));
}
