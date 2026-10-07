import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import { spawnSync } from 'node:child_process';
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
window.__agentRoots=structuredClone(agentReview.roots);
window.__agentCalls=[];window.__agentFailScope=false;window.__agentFailOpen=false;window.__assetReads=0;
const originalInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
 if(cmd==='roots_list')return window.__agentRoots;
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
  window.__agentState={...window.__agentState,scope:args.scope,prompt:template.prompt.replaceAll(template.sessionId,window.__agentState.sessionId),revision:window.__agentState.revision+1};
  return structuredClone(window.__agentState);
 }
 if(cmd==='agentlink_cancel'){
  window.__agentCalls.push(cmd);window.__agentState={...window.__agentState,status:'cancelled',percent:null,revision:window.__agentState.revision+1};
  return structuredClone(window.__agentState);
 }
 if(cmd==='agentlink_new_session'){
  window.__agentCalls.push(cmd);const nextId=crypto.randomUUID();
  window.__agentState={...window.__agentState,status:'waiting',agentName:'',percent:null,prompt:window.__agentState.prompt.replaceAll(window.__agentState.sessionId,nextId),sessionId:nextId,revision:window.__agentState.revision+1};
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
  await page.getByRole('status').filter({hasText:'范围或会话已变化'}).waitFor();
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
  assert.equal(await type.inputValue(),'model');
  assert.equal(await directory.inputValue(),manifest.roots[0].id);
  assert.equal(await page.locator('.agentlink-progress span').textContent(),'100%');
  await page.screenshot({path:join(output,'MBL_AgentLink_Stage3_Finished.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.evaluate(()=>{window.__agentRoots[0].enabled=false;});
  await page.locator('.log-panel').getByRole('button',{name:'刷新资产',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('[aria-label="AgentLink 资产目录"]')?.selectedOptions[0]?.textContent.includes('不可用'));
  assert.equal(await directory.inputValue(),manifest.roots[0].id);
  await page.evaluate(()=>{window.__agentRoots[0].enabled=true;});
  await page.locator('.log-panel').getByRole('button',{name:'刷新资产',exact:true}).click();
  const oldPrompt=await prompt.inputValue();
  await prompt.fill(oldPrompt+'\n继续检查可见标签。');
  await page.getByRole('button',{name:'新会话',exact:true}).click();
  await page.locator('.agentlink-status').filter({hasText:'等待连接'}).waitFor();
  await page.getByRole('status').filter({hasText:'范围或会话已变化'}).waitFor();
  assert.equal(await page.locator('.prompt-panel').getByRole('button',{name:'复制',exact:true}).isDisabled(),true);
  await page.getByRole('button',{name:'重新生成',exact:true}).click();
  const renewedId=await page.evaluate(()=>window.__agentState.sessionId);
  await page.waitForFunction(id=>document.querySelector('.agentlink-prompt')?.value.includes('--session '+id),renewedId);
  assert.equal((await prompt.inputValue()).includes('--session '+manifest.snapshots.finished.sessionId),false);
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
  await fs.writeFile(join(output,'ui-verification.json'),JSON.stringify({synthetic:true,source:'Actual app frontend, Core snapshots, isolated Tauri presentation bridge',checks:{sideBySidePanels:true,promptClipboard:true,editedPromptPersists:true,scopeRegeneration:true,failedScopePreservesValues:true,activeScopeLocked:true,finishedScopePreserved:true,unavailableRootKeepsSelectedScope:true,editedOldSessionPromptBlocked:true,backgroundConnectionAndCompletionRefresh:true,pollingAfterNewSessionAndCancel:true,connectionRetry:true,logTextEscaped:true,manualScrollPreserved:true,followLatest:true,compactViewport:true},pageErrors:errors},null,2));
  await context.close();

  // Record real CSS motion in an isolated page, replaying the synthetic Core trace.
  const motionChecks = {};
  const rawVideo = await fs.mkdtemp(join(output,'motion-video-'));
  const newMotionPage = async (record = false, stage = 'waiting', scheme = 'dark') => {
    const ctx = await browser.newContext({viewport:{width:1480,height:960},locale:'zh-CN',reducedMotion:'no-preference',
      permissions:['clipboard-read','clipboard-write'],...(record ? {recordVideo:{dir:rawVideo,size:{width:1480,height:960}}} : {})});
    await ctx.addInitScript(()=>localStorage.setItem('mmdbridge-motion','true'));
    const view = await ctx.newPage();view.setDefaultTimeout(20000);view.on('pageerror',error=>errors.push(error.message));
    await view.goto(url+'/?stage='+stage+'&scheme='+scheme);
    await view.locator('.asset-card').first().waitFor();await view.getByRole('button',{name:'AgentLink',exact:true}).click();
    await view.waitForFunction(()=>document.querySelector('.agentlink-prompt')?.value.includes('执行要求'));
    await view.evaluate(()=>document.fonts.ready);
    return {ctx,view};
  };
  const recording = await newMotionPage(true);
  const clip = recording.view;
  await clip.waitForTimeout(400);
  await clip.evaluate(active=>{window.__agentState={...active,events:active.events.slice(0,3),percent:30,revision:active.revision-1};},manifest.snapshots.active);
  await clip.locator('.agentlink-status').filter({hasText:'正在接管'}).waitFor();
  await clip.waitForTimeout(1300);
  await clip.evaluate(active=>{window.__agentState=structuredClone(active);},manifest.snapshots.active);
  await clip.waitForFunction(()=>document.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')==='60');
  await clip.waitForTimeout(700);
  await clip.screenshot({path:join(output,'MBL_AgentLink_Motion_Dark.jpg'),type:'jpeg',quality:92});
  await clip.waitForTimeout(900);
  await clip.evaluate(finished=>{window.__agentState=structuredClone(finished);},manifest.snapshots.finished);
  await clip.locator('.agentlink-status').filter({hasText:'已完成'}).waitFor();
  await clip.waitForTimeout(450);
  await clip.screenshot({path:join(output,'MBL_AgentLink_Motion_Finished.jpg'),type:'jpeg',quality:92});
  await clip.waitForTimeout(900);
  const video = clip.video();
  await recording.ctx.close();
  const videoPath = await video.path();
  assert.ok(videoPath.startsWith(rawVideo+'/'));
  const encoded = spawnSync('ffmpeg',['-y','-loglevel','error','-i',videoPath,'-an','-c:v','libx264','-pix_fmt','yuv420p','-crf','22','-movflags','+faststart',join(output,'MBL_AgentLink_Working_20261007.mp4')],{encoding:'utf8'});
  assert.equal(encoded.status,0,encoded.stderr);
  await fs.rm(rawVideo,{recursive:true,force:true});
  motionChecks.recordedStateSequence = true;

  const running = await newMotionPage(false,'active','light');
  const view = running.view;
  const runningLoops = () => view.evaluate(()=>document.querySelector('.agentlink-page').getAnimations({subtree:true}).filter(animation=>animation.playState==='running'&&animation.effect?.getTiming().iterations===Infinity).length);
  await view.waitForFunction(()=>document.querySelector('.agentlink-page')?.dataset.animate==='true');
  assert.equal(await view.locator('.agentlink-event.is-new').count(),0);
  const sampled = await view.evaluate(async()=>{
    const glyph=document.querySelector('.agentlink-activity-orbit');
    const panel=document.querySelector('.log-panel');
    const first=getComputedStyle(glyph).transform,before=panel.getBoundingClientRect().toJSON();
    for(let i=0;i<3;i++)await new Promise(requestAnimationFrame);
    return {first,second:getComputedStyle(glyph).transform,before,after:panel.getBoundingClientRect().toJSON()};
  });
  assert.notEqual(sampled.first,sampled.second);
  assert.ok(await runningLoops()>0 && await runningLoops()<=10);
  assert.deepEqual(sampled.before,sampled.after);
  motionChecks.workingEffectsAdvance = true;motionChecks.layoutStable = true;
  assert.equal(await view.getByRole('progressbar').getAttribute('aria-valuenow'),'60');
  const progress = await view.locator('.agentlink-progress-fill').evaluate(element=>new DOMMatrixReadOnly(getComputedStyle(element).transform).a);
  assert.ok(Math.abs(progress-.6)<.001);
  motionChecks.actualProgressOnly = true;
  await view.waitForTimeout(500);
  await view.screenshot({path:join(output,'MBL_AgentLink_Motion_Light.jpg'),type:'jpeg',quality:92});
  await view.locator('.prompt-panel').getByRole('button',{name:'复制',exact:true}).click();
  assert.equal(await view.evaluate(()=>navigator.clipboard.readText()),await view.getByRole('textbox',{name:'AgentLink Prompt',exact:true}).inputValue());
  motionChecks.promptCopyDuringEffects = true;
  await view.evaluate(()=>{window.__agentState.percent=null;window.__agentState.revision++;});
  await view.waitForFunction(()=>document.querySelector('.agentlink-progress')?.classList.contains('is-indeterminate'));
  assert.equal(await view.getByRole('progressbar').getAttribute('aria-valuenow'),null);
  assert.equal(await view.locator('.agentlink-progress > span').textContent(),'—');
  motionChecks.unknownProgressIndeterminate = true;
  await view.evaluate(()=>{
    const template=window.__agentState.events.at(-1);
    window.__agentState.events.push(...Array.from({length:12},(_,i)=>({...template,id:'motion-check-'+i,message:'隔离动效检查 '+i})));
    window.__agentState.revision++;
  });
  await view.locator('.agentlink-event').filter({hasText:'隔离动效检查 11'}).waitFor();
  const arrivals=await view.locator('.agentlink-event.is-new').allTextContents();
  assert.ok(arrivals.length>0 && arrivals.length<=6 && arrivals.every(text=>text.includes('隔离动效检查')));
  await view.waitForFunction(()=>!document.querySelector('.agentlink-event.is-new'));
  motionChecks.newLogsBounded = true;
  await view.emulateMedia({reducedMotion:'reduce'});
  assert.equal(await runningLoops(),0);
  motionChecks.systemReducedMotion = true;
  await view.emulateMedia({reducedMotion:'no-preference'});
  await view.evaluate(()=>{document.documentElement.dataset.libraryMotion='off';});
  assert.equal(await runningLoops(),0);
  motionChecks.appearanceMotionOff = true;
  await view.evaluate(()=>{document.documentElement.dataset.libraryMotion='on';});
  assert.equal(await view.locator('.agentlink-event.is-new').count(),0);
  assert.ok(await runningLoops()>0);
  motionChecks.historyDoesNotReplay = true;
  await view.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,value:true});document.dispatchEvent(new Event('visibilitychange'));});
  await view.waitForFunction(()=>document.querySelector('.agentlink-page').dataset.animate==='false');
  assert.equal(await runningLoops(),0);
  await view.evaluate(()=>{delete document.hidden;document.dispatchEvent(new Event('visibilitychange'));});
  await view.waitForFunction(()=>document.querySelector('.agentlink-page').dataset.animate==='true');
  assert.ok(await runningLoops()>0);
  await view.locator('.library-home').click();
  assert.equal(await runningLoops(),0);
  await view.getByRole('button',{name:'AgentLink',exact:true}).click();
  assert.ok(await runningLoops()>0);
  motionChecks.backgroundMotionPaused = true;
  await view.evaluate(()=>{window.__agentFailOpen=true;});
  await view.waitForFunction(()=>document.querySelector('.agentlink-page').dataset.status==='disconnected');
  assert.equal(await runningLoops(),0);
  await view.evaluate(()=>{window.__agentFailOpen=false;});
  await view.waitForFunction(()=>document.querySelector('.agentlink-page').dataset.status==='active');
  assert.ok(await runningLoops()>0);
  motionChecks.connectionLossStopsMotion = true;
  await view.getByRole('button',{name:'取消接管',exact:true}).click();
  await view.locator('.agentlink-status').filter({hasText:'已取消'}).waitFor();
  assert.equal(await runningLoops(),0);
  motionChecks.cancelStopsMotion = true;
  await view.getByRole('button',{name:'新会话',exact:true}).click();
  await view.evaluate(active=>{window.__agentState={...active,sessionId:window.__agentState.sessionId,prompt:window.__agentState.prompt,revision:window.__agentState.revision+1};},manifest.snapshots.active);
  await view.waitForFunction(()=>document.querySelector('.agentlink-page').dataset.status==='active');
  await view.evaluate(finished=>{window.__agentState={...finished,sessionId:window.__agentState.sessionId,prompt:window.__agentState.prompt,revision:window.__agentState.revision+1};},manifest.snapshots.finished);
  await view.waitForFunction(()=>document.querySelector('.agentlink-page').classList.contains('is-settling'));
  assert.equal(await runningLoops(),0);
  await view.waitForFunction(()=>!document.querySelector('.agentlink-page').classList.contains('is-settling'));
  await view.locator('.library-home').click();await view.getByRole('button',{name:'AgentLink',exact:true}).click();
  assert.equal(await view.locator('.agentlink-page.is-settling').count(),0);
  motionChecks.completionSettlesOnce = true;
  await running.ctx.close();
  assert.deepEqual(errors,[]);
  await fs.writeFile(join(output,'motion-verification.json'),JSON.stringify({synthetic:true,source:'Actual frontend CSS motion, synthetic Core trace, isolated Tauri presentation bridge',checks:motionChecks,pageErrors:errors},null,2));
} finally {
  await browser?.close();await new Promise(done=>server.close(done));
}
