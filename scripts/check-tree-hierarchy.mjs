const engines=await import(process.env.PLAYWRIGHT_MODULE||'@playwright/test');
import assert from 'node:assert/strict';
import {mkdirSync} from 'node:fs';
import {resolve} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
const root=fileURLToPath(new URL('../',import.meta.url));
const engine=process.argv.includes('--webkit')?'webkit':'chromium';
const output='/tmp/openidea-tree-hierarchy';mkdirSync(output,{recursive:true});
const browser=await engines[engine].launch({headless:true});
try{
 const page=await browser.newPage({viewport:{width:700,height:600}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.addInitScript(()=>{window.sent=[];const listeners=[];window.openidea={postMessage:s=>window.sent.push(JSON.parse(s)),onMessage:f=>listeners.push(f)};window.receive=m=>listeners.forEach(f=>f(JSON.stringify(m)));});
 await page.goto(pathToFileURL(resolve(root,'plugins/ignore/web/resources/hub.html')).href);
 const items=[{key:'g:user',label:'User templates',icon:'folder',expandable:true,expanded:true},{key:'g:lang',label:'Languages and frameworks',icon:'folder',expandable:true,expanded:false},{key:'g:community',label:'Community templates',icon:'folder',expandable:true,expanded:true},
 {key:'u:.gitignore',label:'.gitignore',parent:'g:user',icon:'file-ignored'},{key:'u:api.php',label:'api.php',parent:'g:user',icon:'star'},
 ...Array.from({length:205},(_,i)=>({key:`t:${i}`,label:`Language ${i}`,parent:'g:lang',icon:'file-ignored'})),{key:'g:nested',label:'Nested community',icon:'folder',parent:'g:community',expandable:true,expanded:true},{key:'t:community',label:'Community example',parent:'g:nested',icon:'file-ignored'}];
 const model={title:'Ignore Templates',plugin:'.ignore',toolbar:[{key:'preferences',label:'Preferences',icon:'settings',enabled:true}],body:{kind:'tree',items}};
 const reply=async()=>page.evaluate(model=>{const r=window.sent.at(-1);window.receive({...r,project:'/fixture',model});},model);await reply();
 const keys=()=>page.locator('tbody tr').evaluateAll(rows=>rows.map(n=>n.dataset.resourceKey));
 assert.deepEqual(await keys(),['g:user','u:.gitignore','u:api.php','g:lang','g:community','g:nested','t:community'],'children follow their real parent, regardless of source order');
 assert.equal(await page.locator('th').count(),1);assert.equal(await page.locator('thead').isVisible(),false,'no empty Details column or redundant single-column header');
 const parent=page.locator('tr[data-resource-key="g:user"]'),child=page.locator('tr[data-resource-key="u:.gitignore"]');
 assert.equal(await page.locator('table').getAttribute('role'),'treegrid');
 assert.deepEqual([await parent.getAttribute('aria-level'),await child.getAttribute('aria-level'),await page.locator('tr[data-resource-key="t:community"]').getAttribute('aria-level')],['1','2','3'],'tree rows expose their depth');
 assert.deepEqual([await parent.getAttribute('aria-expanded'),await page.locator('tr[data-resource-key="g:lang"]').getAttribute('aria-expanded'),await child.getAttribute('aria-expanded')],['true','false',null],'only expandable rows expose expansion state');
 assert.equal(await parent.evaluate(n=>n.getBoundingClientRect().height),24);assert.equal(await child.evaluate(n=>n.getBoundingClientRect().height),24);
 const px=await parent.locator('.tree-label').boundingBox(),cx=await child.locator('.tree-label').boundingBox();assert.equal(cx.x-px.x,20,'leaf and disclosure gutters align with one indentation step');
 assert.equal(await child.locator('svg[data-ui-native-name="file-ignored"]').count(),1,'ordinary user templates are files, not favorites');
 assert.equal(await page.locator('[data-ui-menu]').count(),0,'long trees do not allocate a menu per row');
 items[1].expanded=true;await page.getByRole('button',{name:'Expand Languages and frameworks',exact:true}).click();assert.equal(await page.evaluate(()=>window.sent.at(-1).event.key),'g:lang');await reply();
 assert.equal((await keys()).length,212,'all expanded tree items remain in one scrollable tree');assert.equal(await page.locator('.pager').count(),0);
 await page.evaluate(()=>document.documentElement.dataset.theme='dark');await page.screenshot({path:resolve(output,`${engine}-ignore-tree.png`)});
 await page.getByRole('searchbox').fill('.gitignore');assert.deepEqual(await keys(),['g:user','u:.gitignore'],'search retains ancestor context');
 items[0].expanded=false;await reply();assert.deepEqual(await keys(),['g:user','u:.gitignore'],'matching descendants stay visible under collapsed search ancestors');
 await page.getByRole('button',{name:'Clear filters',exact:true}).click();assert.ok(!(await keys()).includes('u:.gitignore'),'clearing search restores collapse state');
 await parent.dblclick();assert.deepEqual(await page.evaluate(()=>window.sent.at(-1).event),{kind:'expand',key:'g:user'});items[0].expanded=true;await reply();
 await child.click({button:'right'});assert.equal(await page.locator('[data-ui-menu]').count(),1);await page.getByRole('menuitem',{name:'Open',exact:true}).click();assert.equal(await page.evaluate(()=>window.sent.at(-1).event.key),'u:.gitignore');await reply();
 // Column sorting orders siblings, never interleaves children of other groups.
 for(const item of items)item.detail='Template source';await page.evaluate(()=>window.receive({channel:'ui-kit-refresh'}));await reply();await page.getByRole('button',{name:'Resource',exact:true}).click();
 const sorted=await keys(),user=sorted.indexOf('g:user');assert.deepEqual(sorted.slice(user,user+3),['g:user','u:.gitignore','u:api.php']);
 assert.deepEqual(errors,[]);console.log(`PASS ${engine}: parent-first tree order, user-template placement, 24px gutters, all 212 rows without paging, ancestor-aware search, collapse/activation, sibling sorting and lazy context menus`);
}finally{await browser.close();}
