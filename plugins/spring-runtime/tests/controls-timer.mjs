import assert from 'node:assert/strict';
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
const source=readFileSync(new URL('../web/resources/runtime-controls.js',import.meta.url),'utf8');
const html=readFileSync(new URL('../web/resources/runtime-controls.html',import.meta.url),'utf8');
const messages=[],timers=new Map(),nodes=new Map(),events={};let next=0,receive,random=0,intersection;
// A deliberately small DOM adapter, not a renderer: only real packaged IDs
// exist, and the Element attribute/class/text APIs used by this driver follow
// DOM string semantics. An unmodeled API must still fail rather than be no-op.
for(const [,attributes,id] of html.matchAll(/<\w+\b([^>]*\bid="([^"]+)"[^>]*)>/g)) {
  assert.ok(!nodes.has(id),`duplicate packaged ID ${id}`);
  const attrs=new Map();const initialClass=/\bclass="([^"]*)"/.exec(attributes)?.[1];
  if(initialClass!==undefined) attrs.set('class',initialClass);
  let content='';
  const element={disabled:/\bdisabled(?:\s|=|$)/.test(attributes),
    setAttribute:(name,value)=>attrs.set(String(name).toLowerCase(),String(value)),
    getAttribute:name=>attrs.get(String(name).toLowerCase())??null,
    get className(){return attrs.get('class')??'';},set className(value){attrs.set('class',String(value));},
    get textContent(){return content;},set textContent(value){content=value==null?'':String(value);},
    addEventListener:(kind,fn)=>events[id+kind]=fn};
  nodes.set(id,element);
}
const node=id=>{assert.ok(nodes.has(id),`missing packaged ID ${id}`);return nodes.get(id);};
const document={documentElement:{isConnected:true},hidden:false,getElementById:id=>nodes.get(id)??null,addEventListener:(kind,fn)=>events[kind]=fn,removeEventListener:(kind,fn)=>{if(events[kind]===fn)delete events[kind];}};
const window={document,addEventListener:(kind,fn)=>events[kind]=fn,removeEventListener:(kind,fn)=>{if(events[kind]===fn)delete events[kind];},openidea:{postMessage:s=>messages.push(JSON.parse(s)),onMessage:fn=>receive=fn}};
vm.runInNewContext(source,{IntersectionObserver:class{constructor(fn){intersection=fn;}observe(){}disconnect(){}},window,document,openidea:window.openidea,crypto:{getRandomValues:b=>b.fill(++random)},Uint8Array,JSON,setTimeout:(fn,ms)=>{assert.equal(ms,60000);timers.set(++next,fn);return next;},clearTimeout:id=>timers.delete(id)});
const last=()=>messages.at(-1);
function reply(model,overrides={}) {receive(JSON.stringify({...last(),model,...overrides}));}
const model={root:'/project',url:'http://localhost:8080/actuator',auth:'none',enabled:true,active:false};
assert.equal(last().op,'hello');assert.equal(timers.size,0);
assert.equal(document.getElementById('not-in-packaged-html'),null);
assert.equal(node('refresh').getAttribute('aria-busy'),'true','initial hello exposes pending bridge work');
reply(model,{frame:'f'.repeat(32)});assert.equal(node('start').disabled,true);
assert.equal(node('refresh').getAttribute('aria-busy'),'true','foreign frame cannot clear busy state');
reply(model);assert.equal(node('start').disabled,true);
assert.equal(node('refresh').getAttribute('aria-busy'),'false');
intersection([{isIntersecting:true,intersectionRatio:1}]);reply(model);assert.equal(node('start').disabled,false);
events.startclick();assert.equal(last().op,'start');const firstNonce=last().nonce;
assert.equal(node('refresh').getAttribute('aria-busy'),'true');assert.equal(node('refresh').disabled,true);
reply({...model,active:true});assert.equal(timers.size,1);
assert.equal(node('refresh').getAttribute('aria-busy'),'false');assert.equal(node('refresh').disabled,false);
reply({...model,active:true},{sequence:last().sequence-1});assert.equal(timers.size,1);
const tick=[...timers.values()][0];timers.clear();tick();assert.equal(last().op,'tick');assert.equal(timers.size,0);
reply({...model,active:true});assert.equal(timers.size,1);
document.hidden=true;events.visibilitychange();assert.equal(timers.size,0);assert.equal(last().op,'stop');reply(model);assert.equal(timers.size,0);
document.hidden=false;events.visibilitychange();assert.equal(last().op,'hello');reply(model);assert.equal(timers.size,0);
events.startclick();assert.notEqual(last().nonce,firstNonce);reply({...model,active:true});
events.pagehide();assert.equal(timers.size,0);assert.equal(last().op,'stop');reply(model);
events.startclick();reply({...model,active:true});const callback=[...timers.values()][0];timers.clear();callback();
document.hidden=true;events.visibilitychange();assert.equal(timers.size,0);reply({...model,active:true});assert.equal(last().op,'stop','a reply arriving hidden cannot rearm');reply(model);assert.equal(timers.size,0);
document.hidden=false;events.visibilitychange();reply(model);events.startclick();reply({...model,active:true});
intersection([{isIntersecting:false,intersectionRatio:0}]);assert.equal(document.hidden,false);assert.equal(timers.size,0);assert.equal(last().op,'stop','CSS-hidden mounted iframe must stop');reply(model);assert.equal(node('start').disabled,true);
intersection([{isIntersecting:true,intersectionRatio:1}]);assert.equal(last().op,'hello');reply(model);assert.equal(timers.size,0);assert.equal(node('start').disabled,false);
// Hiding even briefly during an outstanding tick revokes the lease. Becoming
// visible before its reply must not replace the pending sequence with hello.
events.startclick();reply({...model,active:true});
const briefTick=[...timers.values()][0];timers.clear();briefTick();
const inFlight={...last()};
intersection([{isIntersecting:false,intersectionRatio:0}]);
intersection([{isIntersecting:true,intersectionRatio:1}]);
assert.equal(last().sequence,inFlight.sequence,'brief reopen must preserve the outstanding reply');
reply({...model,active:true});
assert.equal(last().op,'stop','brief hiding during an in-flight tick must stop before rearming');
assert.equal(timers.size,0);
reply(model);assert.equal(timers.size,0);assert.equal(node('start').disabled,false);
// The same race while Start is outstanding cannot grant a later visible tick.
events.startclick();const pendingStart={...last()};
document.hidden=true;events.visibilitychange();
document.hidden=false;events.visibilitychange();
assert.equal(last().sequence,pendingStart.sequence);
reply({...model,active:true});assert.equal(last().op,'stop');assert.equal(timers.size,0);
reply(model);assert.equal(timers.size,0);
// Current packaged status styling is tested separately from historical timer
// receipts. Reply text stays literal; only a matching reply clears pending work.
events.refreshclick();assert.equal(last().op,'refresh');assert.equal(node('refresh').getAttribute('aria-busy'),'true');
const error='<img src=x onerror=unexpected()> observed failure';
reply({...model,active:true,error});
assert.equal(timers.size,0);assert.equal(node('stop').disabled,true);
assert.equal(node('status').className,'ui-alert ui-alert--error');
assert.equal(node('status').getAttribute('class'),'ui-alert ui-alert--error');
assert.equal(node('status').textContent,error,'error is assigned as literal text');
assert.equal(node('refresh').getAttribute('aria-busy'),'false');
events.refreshclick();const pending={...last()};
reply(model,{sequence:pending.sequence-1});receive('not JSON');
reply({...model,root:'/other-project'});
assert.equal(node('refresh').getAttribute('aria-busy'),'true','stale/malformed/foreign-project reply cannot finish pending request');
assert.equal(node('status').className,'ui-alert ui-alert--error');assert.equal(node('status').textContent,error);
reply({...model,poll:'Polling stopped',status:'Polling stopped'});
assert.equal(node('refresh').getAttribute('aria-busy'),'false');
assert.equal(node('status').className,'');assert.equal(node('status').textContent,'','success clears old error and duplicate status');
assert.equal(node('poll').textContent,'Polling stopped');assert.equal(timers.size,0);
events.refreshclick();reply({...model,status:'Current observation',poll:'Polling stopped'});
assert.equal(node('status').textContent,'Current observation');assert.equal(node('status').className,'');
assert.equal(node('refresh').getAttribute('aria-busy'),'false');assert.equal(timers.size,0);
// Spring Runtime: Start Auto-Refresh posts a frame-less start request; it starts once,
// and only through the normal validated Start round trip.
receive(JSON.stringify({channel:'runtime-controls',op:'start-request'}));
assert.equal(last().op,'start');const requestedStart=last().nonce;
reply({...model,active:true});assert.equal(timers.size,1);
events.stopclick();assert.equal(last().op,'stop');reply(model);assert.equal(timers.size,0);
receive(JSON.stringify({channel:'runtime-controls',op:'start-request',frame:'f'.repeat(32)}));assert.equal(last().op,'stop','framed messages are replies, not requests');
events.refreshclick();reply({...model,autostart:true});assert.equal(last().op,'start','a hello/refresh reply can carry the request');assert.notEqual(last().nonce,requestedStart);
reply({...model,active:true});events.stopclick();reply(model);
events.refreshclick();reply({...model,autostart:true,enabled:false});assert.equal(last().op,'refresh','disabled setting drops the request');
console.log('current packaged controls: frame/sequence routing, 60s tick, hidden/busy/pagehide stop, brief hide during tick/start, explicit restart, aria-busy and literal error/status class transitions passed');

// Grouped-view disposal revokes an outstanding Start before releasing listeners.
const dispose=window.uiKitRuntimeControls.mount(document,window.openidea);
intersection([{isIntersecting:true,intersectionRatio:1}]);reply(model);
events.startclick();const unmountedStart={...last()};dispose();
assert.equal(last().op,'stop');assert.ok(last().sequence>unmountedStart.sequence);
assert.equal(timers.size,0);
receive(JSON.stringify({...unmountedStart,model:{...model,active:true}}));
assert.equal(timers.size,0,'retired Start reply cannot rearm an unmounted driver');
console.log('grouped controls disposal: pending Start queues Stop, removes lifecycle listeners and ignores late reply');
