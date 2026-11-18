// Shared group-tab presentation and routing fixtures; this is not live-app acceptance.
const engines=await import(process.env.PLAYWRIGHT_MODULE||'@playwright/test');
import assert from 'node:assert/strict';
import {cpSync,mkdtempSync,writeFileSync,rmSync,mkdirSync,readdirSync,existsSync,readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const engine=process.argv.includes('--webkit')?'webkit':'chromium';
const scratch=mkdtempSync(join(tmpdir(),'openidea-grouped-ui-'));
cpSync(resolve('crates/ui-kit/web'),scratch,{recursive:true});
writeFileSync(join(scratch,'legacy.js'),`window.uiKitLegacy={kube(root,bridge){const source=root.querySelector('#src'),overlay=root.querySelector('#overlay');source.addEventListener('input',()=>overlay.textContent=source.value);bridge.onMessage(text=>{root.querySelector('#reply').textContent=text;});root.querySelector('#apply').onclick=()=>bridge.postMessage(source.value);}};`);
const browser=await engines[engine].launch({headless:true});
try {
 const page=await browser.newPage({viewport:{width:768,height:520}});const errors=[];page.on('pageerror',error=>errors.push(error.message));
 await page.addInitScript(()=>{
  window.sent=[];const listeners=[];
  window.openidea={postMessage:message=>window.sent.push(JSON.parse(message)),onMessage:callback=>listeners.push(callback)};
  window.receive=message=>listeners.forEach(callback=>callback(JSON.stringify(message)));
  window.runtimeMounts=0;window.runtimeStops=0;
  window.uiKitRuntimeControls={mount(root,bridge){window.runtimeMounts++;bridge.onMessage(text=>root.querySelector('#runtime-reply').textContent=text);return()=>{window.runtimeStops++;bridge.postMessage('stop');};}};
 });
 const tabs=[{id:'first',label:'Resources'},{id:'second',label:'Changes'},{id:'third',label:'Request / Response'}];
 const rows={kind:'resources',columns:['Name'],items:[{key:'alpha',cells:['alpha']},{key:'beta',cells:['beta']}]};
 const base=(activeView,body=rows)=>({title:tabs.find(tab=>tab.id===activeView).label,viewTabs:tabs,activeView,toolbar:[],actions:[],contextButtons:[],body});
 const last=()=>page.evaluate(()=>window.sent.at(-1));
 const answer=async(model,request,extra={})=>{request ||= await last();return page.evaluate(({model,request,extra})=>window.receive({...request,project:'/fixture',model,...extra}),{model,request,extra});};
 const open=async(model,packagedTabs=[])=>{await page.goto(pathToFileURL(join(scratch,'hub.html')).href);await page.waitForFunction(()=>window.sent.length>0);assert.equal((await last()).targetView,undefined);await page.evaluate(tabs=>window.uiKitViewTabs=tabs,packagedTabs);await answer(model);};
 const switchTo=async(id,model)=>{const from=await page.locator('#view-tabs [aria-selected=true]').getAttribute('data-view-key');await page.locator(`#view-tabs [data-view-key=${id}]`).click();const request=await last();assert.equal(request.op,'select-view');assert.equal(request.target,id);assert.equal(request.targetView,from);await answer(model,request);return request;};
 await open(base('first'));
 assert.equal(await page.locator('#view-tabs [role=tab]').count(),3);
 assert.equal(await page.locator('#sections').isVisible(),false);
 // Keyboard navigation uses the same kit tab control and target-bound request.
 await page.locator('#view-tabs [data-view-key=first]').focus();await page.keyboard.press('ArrowRight');
 const keyboardRequest=await last();assert.equal(keyboardRequest.target,'second');assert.equal(keyboardRequest.targetView,'first');
 await answer(base('first'),{...keyboardRequest,sequence:keyboardRequest.sequence-1});
 assert.equal(await page.locator('#view-tabs [aria-selected=true]').getAttribute('data-view-key'),'first');
 await answer(base('second'),keyboardRequest);
 assert.equal(await page.locator('#view-tabs [aria-selected=true]').getAttribute('data-view-key'),'second');
 await page.keyboard.press('End');assert.equal((await last()).target,'third');await answer(base('third'));
 await page.keyboard.press('Home');assert.equal((await last()).target,'first');await answer(base('first'));
 // Query/selection state is namespaced by logical view, including equal-shaped bodies.
 await page.locator('#search').fill('alpha');await page.locator('tr[data-resource-key=alpha]').click();assert.equal((await last()).targetView,'first');await answer(base('first'));
 await switchTo('second',base('second'));
 assert.equal(await page.locator('#search').inputValue(),'');assert.equal(await page.locator('tbody tr[aria-selected=true]').count(),0);
 await page.locator('tr[data-resource-key=beta]').dblclick();assert.equal((await last()).targetView,'second');await answer(base('second'));
 await switchTo('first',base('first'));assert.equal(await page.locator('#search').inputValue(),'alpha');assert.equal(await page.locator('tr[data-resource-key=alpha]').getAttribute('aria-selected'),'true');
 // Lists/tables and trees retain native row navigation, selection and focus after refresh.
 await open(base('first'));await page.locator('tr[data-resource-key=alpha]').focus();await page.keyboard.press('ArrowDown');assert.deepEqual((await last()).event,{kind:'select',key:'beta'});
 await answer(base('first',{...rows,items:[...rows.items,{key:'gamma',cells:['gamma']}]}));assert.equal(await page.evaluate(()=>document.activeElement.dataset.resourceKey),'beta');
 await page.keyboard.press('Home');assert.equal((await last()).event.key,'alpha');await answer(base('first'));
 await page.keyboard.press('End');assert.equal((await last()).event.key,'beta');await answer(base('first'));
 assert.equal(await page.locator('tbody tr[tabindex="0"]').count(),1);
 const tree=expanded=>({kind:'tree',items:[{key:'group',label:'Group',expandable:true,expanded},{key:'child',label:'Child',parent:'group'},{key:'other',label:'Other'}]});
 await open(base('first',tree(false)));await page.locator('tr[data-resource-key=group]').focus();await page.keyboard.press('ArrowRight');assert.deepEqual((await last()).event,{kind:'expand',key:'group'});await answer(base('first',tree(true)));
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.resourceKey),'group');await page.keyboard.press('ArrowRight');assert.deepEqual((await last()).event,{kind:'select',key:'child'});await answer(base('first',tree(true)));
 await page.keyboard.press('ArrowLeft');assert.deepEqual((await last()).event,{kind:'select',key:'group'});await answer(base('first',tree(true)));
 await page.keyboard.press('ArrowLeft');assert.deepEqual((await last()).event,{kind:'collapse',key:'group'});await answer(base('first',tree(false)));
 assert.equal(await page.locator('tbody tr[tabindex="0"]').count(),1);
 // Generic row menus use the selected-row request sequence to unlock only
 // authoritative toolbar actions, including rows whose primary activation is unavailable.
 const managed={...base('first',{...rows,items:rows.items.map(row=>({...row,activationEnabled:false}))}),toolbar:[{key:'delete',label:'Delete User Template',enabled:true,icon:'trash'}],contextButtons:['delete']};
 await open(managed);await page.locator('tr[data-resource-key=alpha]').click({button:'right'});
 assert.equal((await last()).event.kind,'select');await answer(managed);
 assert.equal(await page.getByRole('menuitem',{name:'Delete User Template',exact:true}).isVisible(),true);
 await page.getByRole('menuitem',{name:'Delete User Template',exact:true}).click();assert.equal((await last()).op,'item-button');assert.equal((await last()).targetView,'first');await answer(managed);
 // Field syncs are flushed to their old tab before navigation; same-name fields don't bleed.
 const form=value=>({kind:'form',fields:[{key:'name',label:'Name',value:{kind:'text',text:value}}]});
 const withSave=(id)=>({...base(id,form('')),toolbar:[{key:'save',label:'Save',enabled:true}]});
 await open(withSave('first'));
 await page.evaluate(()=>{window.oldButton=document.querySelector('#toolbar button');window.oldField=document.querySelector('[data-field-key=name]');});
 await switchTo('second',withSave('second'));const detachedBefore=await page.evaluate(()=>window.sent.length);
 await page.evaluate(()=>{window.oldButton.click();window.oldField.value='wrong-tab';window.oldField.dispatchEvent(new Event('input'));window.oldField.dispatchEvent(new Event('change'));});
 assert.equal(await page.evaluate(()=>window.sent.length),detachedBefore);assert.equal(await page.getByRole('textbox',{name:'Name',exact:true}).inputValue(),'');
 await open(base('first',form('')));await page.getByRole('textbox',{name:'Name',exact:true}).fill('draft-first');
 const firstNav=await switchTo('second',base('second',form('')));
 const fieldRequest=await page.evaluate(()=>window.sent.find(request=>request.event?.kind==='field'));
 assert.equal(fieldRequest.targetView,'first');assert.equal(fieldRequest.event.value,'draft-first');assert.ok(fieldRequest.sequence<firstNav.sequence);
 assert.equal(await page.getByRole('textbox',{name:'Name',exact:true}).inputValue(),'');
 await page.getByRole('textbox',{name:'Name',exact:true}).fill('draft-second');await switchTo('first',base('first',form('draft-first')));
 assert.equal(await page.getByRole('textbox',{name:'Name',exact:true}).inputValue(),'draft-first');
 await answer(base('second',form('late-wrong-tab')),fieldRequest);assert.equal(await page.getByRole('textbox',{name:'Name',exact:true}).inputValue(),'draft-first');
 await switchTo('second',base('second',form('draft-second')));assert.equal(await page.getByRole('textbox',{name:'Name',exact:true}).inputValue(),'draft-second');
 // Exact-model HTML draft restoration keeps local YAML edits and re-highlights them.
 const html={kind:'html',html:'<textarea id="src">kind: Pod</textarea><pre id="overlay"></pre><output id="reply"></output><button id="apply">Apply</button>'};
 await open(base('first',html));const source=page.locator('.legacy').locator('#src');await source.fill('kind: Deployment');
 await page.evaluate(()=>window.receive({channel:'ui-kit-view-message',frameId:window.uiKitFrameId,targetView:'second',message:'wrong-tab'}));assert.equal(await page.locator('.legacy').locator('#reply').textContent(),'');
 await page.evaluate(()=>window.receive({channel:'ui-kit-view-message',frameId:'wrong-frame',targetView:'first',message:'wrong-frame'}));assert.equal(await page.locator('.legacy').locator('#reply').textContent(),'');
 await page.evaluate(()=>window.receive({channel:'ui-kit-view-message',frameId:window.uiKitFrameId,targetView:'first',message:'correct'}));assert.equal(await page.locator('.legacy').locator('#reply').textContent(),'correct');
 await switchTo('second',base('second'));await switchTo('first',base('first',html));assert.equal(await page.locator('.legacy').locator('#src').inputValue(),'kind: Deployment');assert.equal(await page.locator('.legacy').locator('#overlay').textContent(),'kind: Deployment');
 await page.locator('.legacy').getByRole('button',{name:'Apply',exact:true}).click();assert.equal((await last()).targetView,'first');await answer(base('first',html));
 await switchTo('second',base('second'));await switchTo('first',base('first',{...html,html:html.html.replace('kind: Pod','kind: Service')}));assert.equal(await page.locator('.legacy').locator('#src').inputValue(),'kind: Service');
 // A revoked/back authoritative model removes cached HTML plaintext/drafts.
 await open(base('first',html));await page.locator('.legacy').locator('#src').fill('sensitive old draft');
 await switchTo('second',base('second'));await switchTo('first',base('first',{kind:'message',text:'Access revoked'}));
 await switchTo('second',base('second'));await switchTo('first',base('first',html));assert.equal(await page.locator('.legacy').locator('#src').inputValue(),'kind: Pod');
 // Inactive-tab invalidations don't destroy active HTML controls. Runtime cleanup
 // sends its scoped stop before the navigation event, and background sequence0 never supersedes it.
 await open(base('first',{kind:'html',html:'<div data-runtime-controls><output id="runtime-reply"></output></div>'}));assert.equal(await page.evaluate(()=>window.runtimeMounts),1);
 const before=await page.evaluate(()=>window.sent.length);await page.evaluate(()=>window.receive({channel:'ui-kit-refresh',targetView:'second'}));assert.equal(await page.evaluate(()=>window.sent.length),before);
 await switchTo('second',base('second'));
 const messages=await page.evaluate(()=>window.sent),stop=messages.find(request=>request.op==='opaque-message');assert.equal(stop.message,'stop');assert.equal(stop.targetView,'first');assert.equal(stop.sequence,0);assert.equal(messages.at(-1).op,'select-view');assert.equal(await page.evaluate(()=>window.runtimeStops),1);
 // Compact themes/viewport combinations keep outer tabs reachable without root overflow.
 const output=process.env.UI_QA_DIR;if(output)mkdirSync(output,{recursive:true});
 for(const width of [320,768])for(const theme of ['light','dark']) {
  await page.setViewportSize({width,height:520});await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
  if(output)await page.screenshot({path:join(output,`${engine}-group-tabs-${theme}-${width}.png`)});
 }
 // Optional packaged sections show one cluster of tabs behind a compact picker; reveal_view follows the active view.
 await page.goto(pathToFileURL(join(scratch,'hub.html')).href);await page.waitForFunction(()=>window.sent.length>0);
 await page.evaluate(()=>window.uiKitViewSections=[{id:'browse',label:'Browse',tabs:['first','second']},{id:'run',label:'Run',tabs:['third']}]);await answer(base('first'));
 const shownTabs=()=>page.locator('#view-tabs [role=tab]').evaluateAll(nodes=>nodes.map(node=>node.dataset.viewKey));
 const picker=page.getByRole('combobox',{name:'View group',exact:true});
 assert.deepEqual(await shownTabs(),['first','second']);assert.equal(await picker.textContent(),'Browse');
 await switchTo('second',base('second'));
 await picker.click();await page.getByRole('option',{name:'Run',exact:true}).click();
 let sectionRequest=await last();assert.equal(sectionRequest.op,'select-view');assert.equal(sectionRequest.target,'third');await answer(base('third'),sectionRequest);
 assert.deepEqual(await shownTabs(),['third']);assert.equal(await picker.textContent(),'Run');
 await picker.focus();await page.keyboard.press('Enter');await page.keyboard.press('Home');await page.keyboard.press('Enter');
 sectionRequest=await last();assert.equal(sectionRequest.target,'second','a section reopens on its last tab');await answer(base('second'),sectionRequest);
 await answer(base('third'));assert.deepEqual(await shownTabs(),['third']);assert.equal(await picker.textContent(),'Run');
 await page.setViewportSize({width:320,height:520});assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));await page.setViewportSize({width:768,height:520});
 await open(base('first'));assert.equal(await page.locator('.view-section-picker').isVisible(),false,'groups without sections keep flat tabs');
 // Resource tables are grids with a visible row focus; filter counts are announced once settled.
 assert.equal(await page.locator('table').getAttribute('role'),'grid');
 await page.locator('tr[data-resource-key=alpha]').focus();await page.keyboard.press('ArrowDown');
 assert.equal(await page.locator('tr[data-resource-key=beta]').evaluate(node=>getComputedStyle(node).outlineStyle),'solid');await answer(base('first'));
 const statusBefore=await page.locator('#status').textContent();await page.locator('#search').pressSequentially('alp');
 assert.equal(await page.locator('#status').textContent(),statusBefore,'typing does not rewrite the live region per keystroke');
 await page.waitForFunction(()=>document.querySelector('#status').textContent.startsWith('1 of 2'));await page.locator('#search').fill('');
 // Narrow docks ellipsize resource identifiers instead of scrolling the table sideways.
 const long={kind:'resources',columns:['Path','Name','Handler'],items:[{key:'long',cells:['/api/v1/organisations/{organisation}/projects/{project}/environments','organisation.project.environments.index','App\\Http\\Controllers\\EnvironmentController@index']}]};
 await answer(base('first',long));await page.setViewportSize({width:320,height:520});
 assert.ok(await page.locator('#content').evaluate(node=>node.scrollWidth<=node.clientWidth+1),'no horizontal table scroll at 320px');
 assert.equal(await page.locator('td').first().getAttribute('title'),long.items[0].cells[0]);await page.setViewportSize({width:768,height:520});
 // Failed loads are distinguishable from empty projects and retry the render; denied loads name the permission.
 await page.goto(pathToFileURL(join(scratch,'hub.html')).href);await page.waitForFunction(()=>window.sent.length>0);
 await page.evaluate(request=>window.receive({...request,error:'disk unavailable'}),await last());
 assert.equal(await page.locator('.ui-empty[data-state=error]').isVisible(),true);assert.match(await page.locator('#content').textContent(),/disk unavailable/);
 await page.getByRole('button',{name:'Retry',exact:true}).click();assert.equal((await last()).op,'render');
 await page.evaluate(request=>window.receive({...request,error:'permission denied: project.read is not granted',denied:'project.read'}),await last());
 assert.match(await page.locator('.ui-empty[data-state=denied]').textContent(),/project\.read.*Settings › Plugins/);
 await page.getByRole('button',{name:'Retry',exact:true}).click();await answer(base('first'));assert.equal(await page.locator('.ui-empty').count(),0);assert.equal(await page.locator('tbody tr').count(),2);
 await answer(base('first',{...rows,items:[]}));assert.equal(await page.locator('.ui-empty[data-state=empty]').count(),1);
 // Project the shipped group definitions, preserving every logical tab label/id.
 let groups=0;
 for(const plugin of readdirSync('plugins')) {
  const path=resolve('plugins',plugin,'src/ui_config.json');if(!existsSync(path))continue;
  const config=JSON.parse(readFileSync(path,'utf8'));if(!config.group)continue;groups++;
  const counts=[211,11,1566,2290,14,0];
  const modelFor=id=>({title:config.views[id],activeView:id,viewTabs:config.group.tabs.map((tab,index)=>plugin==='laravel' ? {id:tab.id,label:tab.label,count:counts[index]} : tab),toolbar:[],actions:[],body:{kind:'message',text:`${plugin}/${id}`}});
  await open(modelFor(config.group.primary),config.group.tabs);
  assert.deepEqual(await page.locator('#view-tabs [role=tab]').evaluateAll(nodes=>nodes.map(node=>[...node.childNodes].filter(child=>child.nodeType===Node.TEXT_NODE).map(child=>child.textContent).join('').trim())),config.group.tabs.map(tab=>tab.label),plugin);
  if(plugin==='laravel') {
   assert.deepEqual(await page.locator('#view-tabs .tab-count').allTextContents(),counts.map(String));
   assert.equal(await page.locator('#view-tabs [data-view-key=laravel-env]').getAttribute('aria-label'),'Env (0)');
   assert.notEqual(await page.locator('#view-tabs .tab-count').first().evaluate(node=>getComputedStyle(node).color),await page.locator('#view-tabs button').first().evaluate(node=>getComputedStyle(node).color));
  }
  if(plugin==='laravel')for(const width of [320,768])for(const theme of ['light','dark']) {
   await page.setViewportSize({width,height:520});await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
   assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
   if(output)await page.screenshot({path:join(output,`${engine}-laravel-tabs-${theme}-${width}.png`)});
  }
  for(const tab of config.group.tabs) {
   assert.ok(config.views[tab.id],`${plugin}: declared logical view ${tab.id}`);
   if(tab.icon)assert.ok(await page.locator(`#view-tabs [data-view-key="${tab.id}"] .view-icon`).count(),`${plugin}/${tab.id}: tab icon`);
   if(tab.id !== config.group.primary)await switchTo(tab.id,modelFor(tab.id));
   assert.equal(await page.locator('#view-tabs [aria-selected=true]').getAttribute('data-view-key'),tab.id);
  }
 }
 assert.ok(groups>=8,`expected all 8 migrated plugin groups, found ${groups}`);
 assert.deepEqual(errors,[]);console.log(`PASS ${engine}: grouped tabs, scoped state/events, stale replies, HTML drafts, runtime lifecycle and layout`);
} finally {await browser.close();rmSync(scratch,{recursive:true,force:true});}
