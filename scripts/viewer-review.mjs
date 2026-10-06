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
const fixtures = JSON.parse(await fs.readFile(join(output,'viewer-fixtures.json'),'utf8'));
const compatibility = JSON.parse(await fs.readFile(join(output,'manifest.json'),'utf8'));
assert.equal(fixtures.synthetic,true);
const cutout = compatibility.entries.find(({asset})=>asset.name==='PMX-Cutout');assert.ok(cutout);
const actor = { ...compatibility.entries[0].asset, id:'viewer-character', name:'角色与 Matcap',
  primarySource:fixtures.referencePmx, assetType:'model' };
const assets = [actor,fixtures.scene,fixtures.motion].map(asset=>({...asset,rootId:`root-${asset.assetType==='model'?0:asset.assetType==='motion'?1:2}`,
  statuses:['Ready'],hasThumbnail:false,cardStatus:'CardValid'}));

// Test instrumentation lives only in this isolated bundle.
await build({stdin:{contents:`
  import * as THREE from './src/vendor/three.module.js';
  const add=THREE.Scene.prototype.add;
  THREE.Scene.prototype.add=function(...objects){window.__reviewScene=this;return add.apply(this,objects);};
  const project=THREE.PerspectiveCamera.prototype.updateProjectionMatrix;
  THREE.PerspectiveCamera.prototype.updateProjectionMatrix=function(){window.__reviewCamera=this;return project.call(this);};
  import('./src/main.tsx');`,resolveDir:join(root,'apps/desktop'),loader:'ts'},
  bundle:true,format:'esm',outfile:join(output,'viewer-review.js'),minify:true,logLevel:'silent'});
const [js,css,base] = await Promise.all([
  fs.readFile(join(output,'viewer-review.js'),'utf8'),fs.readFile(join(output,'viewer-review.css'),'utf8'),
  fs.readFile(join(root,'scripts/visual-review-fixture.js'),'utf8'),
]);
const bridge = `${base}
const viewerAssets=${JSON.stringify(assets)};
const viewerFixtures=${JSON.stringify(fixtures)};
window.__reviewCalls=[];
window.__referencePath=viewerFixtures.referencePmx;
window.__actorPreview='viewer-character-pmx.bin';
const originalInvoke=window.__TAURI_INTERNALS__.invoke;
window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
 window.__reviewCalls.push({cmd,args});
 if(cmd==='asset_counts')return {all:3,model:1,motion:1,scene:1,byRoot:{'root-0':1,'root-1':1,'root-2':1}};
 if(cmd==='assets_page')return {items:viewerAssets.filter(a=>(!args.assetType||a.assetType===args.assetType)&&(!args.rootId||a.rootId===args.rootId)),nextCursor:null};
 if(cmd==='asset_inspect')return viewerAssets.find(a=>a.id===args.assetId);
 if(cmd==='model_preview')return (await fetch('/'+window.__actorPreview)).arrayBuffer();
 if(cmd==='model_preview_texture_file')return (await fetch('/viewer-cutout-texture.bin')).arrayBuffer();
 if(cmd==='scene_preview')return (await fetch('/viewer-scene.bin')).arrayBuffer();
 if(cmd==='motion_preview_model_get')return window.__referencePath;
 if(cmd==='model_preview_file'){
  if(window.__referenceDelay)await new Promise(resolve=>setTimeout(resolve,window.__referenceDelay));
  if(window.__referenceFail)throw new Error('预览模型无法读取');
  return (await fetch(args.path.toLowerCase().endsWith('.pmd')?'/viewer-character-pmd.bin':'/viewer-character-pmx.bin')).arrayBuffer();
 }
 if(cmd==='motion_preview_frame')return viewerFixtures.frames[Math.min(60,Math.max(0,Math.round(args.frame)))];
 return originalInvoke(cmd,args);
};`;
const html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>MBL</title><style id="library-style-nonce"></style><style>${css}</style></head><body><div id="root"></div><script>${bridge}</script><script type="module">${js.replaceAll('</script','<\\/script')}</script></body></html>`;
const files = new Map();
for(const name of ['viewer-character-pmx.bin','viewer-character-pmd.bin','viewer-scene.bin','viewer-cutout-texture.bin',`${cutout.asset.id}.bin`]) {
  files.set('/'+name,await fs.readFile(join(output,name)));
}
const server = http.createServer((request,response)=>{
  const bytes=files.get(new URL(request.url,'http://localhost').pathname);
  response.writeHead(200,{'Content-Type':bytes?'application/octet-stream':'text/html; charset=utf-8'});response.end(bytes??html);
});
await new Promise((ready,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',ready);});
let browser, page;
const checks = {};
try {
  browser=await chromium.launch({headless:true,args:['--use-angle=swiftshader','--enable-unsafe-swiftshader']});
  page=await browser.newPage({viewport:{width:1480,height:960},locale:'zh-CN',reducedMotion:'reduce'});
  page.setDefaultTimeout(20000);
  const errors=[]; page.on('pageerror',error=>errors.push(error.message));
  page.on('console',message=>{if(message.type()==='error'&&/THREE\.WebGLProgram|VALIDATE_STATUS|shader error/i.test(message.text()))errors.push(message.text());});
  const url=`http://127.0.0.1:${server.address().port}`;
  const settled = async () => {await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));};
  const snapshot = async () => {
    await settled();
    return page.evaluate(()=>{
      const meshes=[];window.__reviewScene.traverse(node=>{if(node.isMesh)meshes.push({name:node.name,uuid:node.uuid,
        geometry:node.geometry.uuid,position:node.position.toArray(),scale:node.scale.toArray(),matrix:node.matrixWorld.toArray(),
        bones:node.skeleton?.bones.map(bone=>bone.matrix.toArray()),material:node.material.type??node.material[0]?.type});});
      const camera=window.__reviewCamera;
      return {meshes,camera:{position:camera.position.toArray(),quaternion:camera.quaternion.toArray(),fov:camera.fov},
        loads:window.__reviewCalls.filter(call=>['model_preview','scene_preview','model_preview_file','motion_preview_frame'].includes(call.cmd)).length};
    });
  };
  const pixels = async () => {
    await settled();
    return page.evaluate(()=>{
      const source=document.querySelector('[role=dialog] canvas');
      const canvas=document.createElement('canvas');canvas.width=80;canvas.height=80;
      const ctx=canvas.getContext('2d');ctx.drawImage(source,0,0,80,80);
      return [...ctx.getImageData(0,0,80,80).data];
    });
  };
  const difference = (a,b) => a.reduce((total,value,index)=>total+Math.abs(value-b[index]),0);
  const openAsset = async (name) => {await page.getByRole('button',{name:new RegExp(name)}).first().dblclick();await page.locator('[role=dialog] canvas').waitFor();};
  await page.goto(`${url}/?scheme=dark`);
  await page.locator('.asset-card').first().waitFor();await page.evaluate(()=>document.fonts.ready);
  await openAsset(actor.name);
  const actorSelect=page.getByRole('combobox',{name:'角色 Matcap 预设'});
  await actorSelect.waitFor();await page.waitForFunction(()=>!document.querySelector('[aria-label="角色 Matcap 预设"]').disabled);
  const baseline=await snapshot();const baselinePixels=await pixels();
  const finishes={};
  for(const preset of ['soft','ceramic','silver','copper']) {
    await actorSelect.selectOption(preset);const view=await snapshot();const image=await pixels();
    assert.deepEqual(view.camera,baseline.camera);assert.equal(view.meshes[0].geometry,baseline.meshes[0].geometry);
    assert.equal(view.loads,baseline.loads);assert.equal(view.meshes[0].material,'MeshMatcapMaterial');
    const delta=difference(baselinePixels,image);assert.ok(delta>10000,`${preset} did not change the canvas`);finishes[preset]=delta;
  }
  await actorSelect.selectOption('ceramic');
  await page.screenshot({path:join(output,'MBL_Viewer_Stage1_Character_Matcap.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.getByRole('button',{name:'权重类型',exact:true}).click();
  assert.equal(await actorSelect.isDisabled(),true);assert.equal((await snapshot()).meshes[0].material,'MeshStandardMaterial');
  await page.getByRole('button',{name:'材质贴图',exact:true}).click();
  assert.equal(await actorSelect.inputValue(),'ceramic');assert.equal((await snapshot()).meshes[0].material,'MeshMatcapMaterial');
  await actorSelect.selectOption('original');assert.ok(difference(baselinePixels,await pixels())<1000);
  checks.character={finishes,geometryAndCameraPreserved:true,weightColoursPreserved:true,originalRestored:true};
  await page.getByRole('button',{name:'关闭 3D 预览',exact:true}).click();

  await page.evaluate(name=>{window.__actorPreview=name;},`${cutout.asset.id}.bin`);
  await openAsset(actor.name);await page.getByText('已加载贴图 1 / 1',{exact:true}).waitFor();
  await page.evaluate(()=>window.__reviewScene.traverse(node=>{if(node.type==='GridHelper')node.visible=false;}));
  const cutoutBefore=await pixels();
  await actorSelect.selectOption('ceramic');const cutoutAfter=await pixels();
  const foreground = rgba => Array.from({length:rgba.length/4},(_,index)=>
    Math.abs(rgba[index*4]-rgba[0])+Math.abs(rgba[index*4+1]-rgba[1])+Math.abs(rgba[index*4+2]-rgba[2])>50);
  const beforeMask=foreground(cutoutBefore),afterMask=foreground(cutoutAfter);
  let overlap=0,union=0;
  beforeMask.forEach((visible,index)=>{if(visible&&afterMask[index])overlap++;if(visible||afterMask[index])union++;});
  assert.ok(union>300&&union<6000);assert.ok(overlap/union>0.95,'Matcap changed the cutout silhouette');
  checks.character.cutoutSilhouetteIoU=overlap/union;
  await page.getByRole('button',{name:'关闭 3D 预览',exact:true}).click();
  await page.evaluate(()=>{window.__actorPreview='viewer-character-pmx.bin';});

  await openAsset(fixtures.motion.name);
  const motionSelect=page.getByRole('combobox',{name:'动作 Matcap 预设'});
  await page.waitForFunction(()=>!document.querySelector('[aria-label="动作 Matcap 预设"]').disabled);
  const timeline=page.getByRole('slider',{name:'VMD 时间轴'});
  await timeline.focus();await timeline.press('ArrowRight');await timeline.press('ArrowRight');
  await page.waitForFunction(()=>document.querySelector('.motion-viewer-timeline > span')?.textContent==='2 / 60 帧');
  const frameBefore=await snapshot();const motionBefore=await pixels();
  await motionSelect.selectOption('silver');const frameAfter=await snapshot();
  assert.deepEqual(frameAfter.camera,frameBefore.camera);assert.equal(frameAfter.loads,frameBefore.loads);
  assert.deepEqual(frameAfter.meshes[0].matrix,frameBefore.meshes[0].matrix);
  assert.deepEqual(frameAfter.meshes[0].bones,frameBefore.meshes[0].bones);
  assert.equal(await timeline.getAttribute('aria-valuenow'),'2');assert.ok(difference(motionBefore,await pixels())>10000);
  await page.screenshot({path:join(output,'MBL_Viewer_Stage2_Motion_Matcap.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await page.getByRole('button',{name:'播放',exact:true}).click();
  await page.waitForFunction(()=>Number(document.querySelector('[aria-label="VMD 时间轴"]').getAttribute('aria-valuenow'))>=5);
  await motionSelect.selectOption('copper');assert.equal(await page.getByRole('button',{name:'暂停',exact:true}).count(),1);
  await page.waitForFunction(()=>Number(document.querySelector('[aria-label="VMD 时间轴"]').getAttribute('aria-valuenow'))>=8);
  await page.getByRole('button',{name:'暂停',exact:true}).click();
  checks.motion={framePreserved:true,playbackContinued:true,cameraAndGeometryPreserved:true};
  await page.getByRole('button',{name:'关闭动作预览',exact:true}).click();

  await openAsset(fixtures.scene.name);
  const sceneSelect=page.getByRole('combobox',{name:'场景渲染预设'});
  await page.getByRole('status').filter({hasText:'原点 (0, 0, 0)'}).waitFor();
  await page.getByRole('button',{name:'查看原点',exact:true}).click();
  const originalScene=await snapshot();const originalScenePixels=await pixels();
  const gridToggle=page.getByRole('checkbox',{name:'地面网格'});assert.equal(await gridToggle.isChecked(),false);
  await gridToggle.check();const withGrid=await pixels();assert.ok(difference(originalScenePixels,withGrid)>5000);
  await gridToggle.uncheck();
  const reference=originalScene.meshes.find(mesh=>mesh.name==='场景原点参照角色');assert.ok(reference);
  assert.deepEqual(reference.position,[0,0,0]);assert.deepEqual(reference.scale,[1,1,1]);
  const looks={};
  for(const preset of ['daylight','warm','night','studio']) {
    await sceneSelect.selectOption(preset);const state=await snapshot();
    assert.deepEqual(state.camera,originalScene.camera);assert.equal(state.loads,originalScene.loads);
    assert.deepEqual(state.meshes.map(mesh=>mesh.geometry),originalScene.meshes.map(mesh=>mesh.geometry));
    const delta=difference(originalScenePixels,await pixels());assert.ok(delta>10000);looks[preset]=delta;
  }
  await sceneSelect.selectOption('daylight');
  await page.screenshot({path:join(output,'MBL_Viewer_Stage3_Scene_Daylight.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  await sceneSelect.selectOption('night');
  await page.screenshot({path:join(output,'MBL_Viewer_Stage4_Scene_Night.jpg'),type:'jpeg',quality:92,animations:'disabled'});
  const visibleScene=await pixels();
  const referenceToggle=page.getByRole('checkbox',{name:'原点参照角色'});
  await referenceToggle.uncheck();assert.ok(!(await snapshot()).meshes.some(mesh=>mesh.name==='场景原点参照角色'));
  assert.ok(difference(visibleScene,await pixels())>5000);
  await page.evaluate(()=>{window.__referencePath=window.__referencePath.replace('.pmx','.pmd');});
  await referenceToggle.check();await page.getByRole('status').filter({hasText:'参照角色.pmd'}).waitFor();
  assert.equal((await snapshot()).meshes.filter(mesh=>mesh.name==='场景原点参照角色').length,1);
  await page.evaluate(()=>{window.__referenceFail=true;});
  await page.getByRole('button',{name:'重载角色',exact:true}).click();
  await page.getByRole('status').filter({hasText:'参照角色加载失败'}).waitFor();
  assert.equal(await sceneSelect.isDisabled(),false);await sceneSelect.selectOption('warm');
  await page.evaluate(()=>{window.__referenceFail=false;window.__referencePath=null;});
  await page.getByRole('button',{name:'重载角色',exact:true}).click();
  await page.getByRole('status').filter({hasText:'未设置预览角色'}).waitFor();
  await page.evaluate(()=>{window.__referencePath='E:\\MMD\\Tests\\参照角色.pmx';window.__referenceDelay=300;});
  const previousLoads=await page.evaluate(()=>window.__reviewCalls.filter(call=>call.cmd==='model_preview_file').length);
  await page.getByRole('button',{name:'重载角色',exact:true}).click();
  await page.getByRole('status').filter({hasText:'正在加载参照角色'}).waitFor();
  assert.equal(await page.getByRole('button',{name:'重载角色',exact:true}).isDisabled(),true);
  await page.waitForFunction(count=>window.__reviewCalls.filter(call=>call.cmd==='model_preview_file').length>count,previousLoads);
  await referenceToggle.uncheck();
  await page.waitForTimeout(450);
  assert.ok(!(await snapshot()).meshes.some(mesh=>mesh.name==='场景原点参照角色'));
  await page.getByRole('button',{name:'关闭 3D 预览',exact:true}).click();
  await page.evaluate(()=>{window.__referenceDelay=0;});
  await openAsset(fixtures.scene.name);await page.getByRole('status').filter({hasText:'原点 (0, 0, 0)'}).waitFor();
  assert.equal(await sceneSelect.inputValue(),'warm');assert.equal((await snapshot()).meshes.filter(mesh=>mesh.name==='场景原点参照角色').length,1);
  checks.scene={looks,origin:[0,0,0],scale:[1,1,1],pmxAndPmdLoaded:true,hiddenAndReloaded:true,
    missingSettingAndReadFailureRecovered:true,lateLoadDiscarded:true,reopenedWithoutDuplicates:true,presetRemembered:true,gridToggleVerified:true};
  await page.getByRole('button',{name:'关闭 3D 预览',exact:true}).click();
  assert.deepEqual(errors,[]);
  await fs.writeFile(join(output,'viewer-verification.json'),JSON.stringify({sourceCommit:process.env.GITHUB_SHA,
    synthetic:true,headless:true,softwareWebGL:true,checks,pageErrors:errors},null,2));
  console.log('Matcap, scene looks, motion continuity, origin reference and async recovery verified.');
} catch(reason) {
  if(page){await page.screenshot({path:join(output,'viewer-failure.jpg'),type:'jpeg',quality:90}).catch(()=>{});
    await fs.writeFile(join(output,'viewer-failure.txt'),await page.locator('body').innerText().catch(()=>''));}
  throw reason;
} finally {await browser?.close();await new Promise(resolve=>server.close(resolve));}
