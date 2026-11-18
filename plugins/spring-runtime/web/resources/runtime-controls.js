/* Private view driver: no HTTP, no credentials, no persistent timer authority. */
(() => {
  "use strict";
  function mount(mountRoot, openidea) {
  const ownerDocument = mountRoot.ownerDocument || mountRoot;
  const ownerWindow = ownerDocument.defaultView || window;
  const observed = mountRoot.host || mountRoot.documentElement;
  const document = ownerDocument;
  let disposed = false;
  const random = () => { const b = new Uint8Array(16); crypto.getRandomValues(b); return Array.from(b, x => x.toString(16).padStart(2,"0")).join(""); };
  const frame = random(); let nonce = random(), sequence = 0, root = null, url = null, auth = null, timer = null, active = false, busy = false, enabled = false, intersecting = false, stopPending = false, startWanted = false;
  const visible = () => !disposed && intersecting && !document.hidden && observed.isConnected;
  const $ = id => mountRoot.getElementById(id);
  function clear() { if(timer !== null) clearTimeout(timer); timer = null; }
  function buttons() {
    $("start").disabled = busy || active || !enabled || !root || !visible();
    $("stop").disabled = busy || !active; $("refresh").disabled = busy || !root;
    if(!busy) {$("start").hidden = active || !enabled || !root || !visible(); $("stop").hidden = !active; $("refresh").hidden = !root;}
    $("refresh").setAttribute("aria-busy",String(busy));
  }
  function send(op) {
    if(!openidea) { $("status").textContent = "Open in OpenIDEA to use scoped controls."; return; }
    clear(); busy = true; buttons();
    openidea.postMessage(JSON.stringify({channel:"runtime-controls",frame,nonce,sequence:++sequence,root,url,auth,op}));
  }
  // Spring Runtime: Start Auto-Refresh asks for a Start; Rust still validates it.
  function startIfWanted() {
    if(!startWanted || busy || active || !enabled || !root || !visible()) return false;
    startWanted=false; nonce=random(); send("start"); return true;
  }
  function arm() { clear(); if(active && !busy && visible()) timer = setTimeout(() => { timer=null; if(visible() && active) send("tick"); },60000); }
  openidea?.onMessage(raw => {
    if(disposed) return;
    let reply; try { reply=typeof raw === "string" ? JSON.parse(raw) : raw; } catch { return; }
    if(reply.channel === "runtime-controls" && reply.op === "start-request" && reply.frame === undefined) { startWanted=true; startIfWanted(); return; }
    if(reply.channel !== "runtime-controls" || reply.frame !== frame || reply.nonce !== nonce || reply.sequence !== sequence) return;
    const model=reply.model; if(!model || (root !== null && model.root && root !== model.root)) return;
    busy=false; active=model.active === true; enabled=model.enabled === true;
    $("status").className = model.error ? "ui-alert ui-alert--error" : "";
    if(model.error) { active=false; $("status").textContent=model.error; }
    else { root=model.root; url=model.url; auth=model.auth; for(const key of ["root","url","auth","bounds","poll","status"]) $(key).textContent=key === "status" && model.status === model.poll ? "" : model[key] || ""; }
    if((!visible() || stopPending) && active) { active=false; stopPending=false; send("stop"); return; }
    stopPending=false;
    if(model.autostart === true) startWanted=true;
    if(!enabled) startWanted=false;
    if(startIfWanted()) return;
    buttons(); arm();
  });
  $("start").addEventListener("click",() => { nonce=random(); send("start"); });
  $("stop").addEventListener("click",() => { startWanted=false; send("stop"); });
  $("refresh").addEventListener("click",() => send("refresh"));
  function visibilityChanged() {
    clear();
    if(!visible()) {
      // A tick/start can be outstanding when a dock is hidden. Remember the
      // revocation even if the dock reopens before its reply arrives.
      stopPending = stopPending || active || busy;
      if(active && !busy) { stopPending=false; send("stop"); }
      active=false; buttons();
    } else if(!busy) { active=false; send("hello"); }
    // Preserve the pending sequence while busy; its reply must deliver Stop
    // before any visible hello can report/rearm the old lease.
  }
  document.addEventListener("visibilitychange",visibilityChanged);
  // Dock hiding need not change document.hidden or unmount the iframe. Observe
  // actual intersection through the embedding viewport, including hidden docks.
  const observer = typeof IntersectionObserver === "function" ? new IntersectionObserver(entries => {
    const next=entries.some(entry => entry.isIntersecting && entry.intersectionRatio > 0);
    if(next !== intersecting) {intersecting=next;visibilityChanged();}
  }) : null;
  observer?.observe(observed);
  const leave = () => { clear(); if((active || busy) && root) send("stop"); active=false; };
  ownerWindow.addEventListener("pagehide",leave);
  send("hello");
  return () => {
    if(disposed) return;
    leave(); disposed=true; observer?.disconnect();
    document.removeEventListener("visibilitychange",visibilityChanged);
    ownerWindow.removeEventListener("pagehide",leave);
  };
  }
  window.uiKitRuntimeControls = Object.freeze({mount});
  if(document.getElementById("poll") && document.getElementById("start")) mount(document,window.openidea);
})();
