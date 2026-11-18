const engines=await import(process.env.PLAYWRIGHT_MODULE||'@playwright/test');
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const engine=process.argv.includes('--webkit')?'webkit':'chromium';
const browser=await engines[engine].launch({headless:true});
try {
 const page=await browser.newPage();
 await page.addInitScript(()=>{window.sent=[];const listeners=[];window.openidea={postMessage:s=>window.sent.push(JSON.parse(s)),onMessage:f=>listeners.push(f)};window.receive=m=>listeners.forEach(f=>f(JSON.stringify(m)));});
 await page.goto(pathToFileURL(resolve('plugins/dependency-audit/web/resources/hub.html')).href);
 const config=JSON.parse(readFileSync('plugins/dependency-audit/src/ui_config.json','utf8'));
 for(const action of config.actions)action.replacesOpen=action.replaces_open === true;
 const model={title:'Vulnerable Dependencies',toolbar:[],actions:config.actions,body:{kind:'resources',columns:['Type','Dependency'],facets:[],items:[{key:'dep:package',cells:['Dependency','example']},{key:'advisory:package|GHSA-example',cells:['Advisory','example']}]}};
 const reply=()=>page.evaluate(model=>{const r=window.sent.at(-1);window.receive({...r,project:'/fixture',model});},model);
 await reply();await page.locator('tbody tr').nth(0).click({button:'right'});
 assert.equal(await page.getByRole('menuitem',{name:'Open',exact:true}).count(),1,'dependency source navigation is distinct from advisory navigation');
 for(const name of ['Preview Upgrade…','Open Advisory on OSV.dev…','Ignore Finding with Reason…'])assert.equal(await page.getByRole('menuitem',{name,exact:true}).isVisible(),true);
 await page.getByRole('menuitem',{name:'Open Advisory on OSV.dev…',exact:true}).click();
 assert.deepEqual(await page.evaluate(()=>{const r=window.sent.at(-1);return {op:r.op,id:r.id,key:r.key};}),{op:'command',id:'open-advisory',key:'dep:package'});
 await reply();await page.locator('tbody tr').nth(1).click({button:'right'});
 assert.equal(await page.getByRole('menuitem',{name:'Preview Upgrade…',exact:true}).count(),0);
 assert.equal(await page.getByRole('menuitem',{name:'Open',exact:true}).count(),0,'explicit advisory action replaces duplicate generic Open');
 assert.equal(await page.getByRole('menuitem',{name:'Ignore Finding with Reason…',exact:true}).isVisible(),true);
 for(const [name,icon]of [['Open Advisory on OSV.dev…','external-link'],['Ignore Finding with Reason…','hide']])assert.equal(await page.getByRole('menuitem',{name,exact:true}).locator('svg').getAttribute('data-ui-icon-name'),icon);
 console.log(`PASS ${engine}: dependency menus preserve upgrade and expose advisory/ignore workflows; advisory rows keep their exact-finding actions`);
} finally {await browser.close();}
