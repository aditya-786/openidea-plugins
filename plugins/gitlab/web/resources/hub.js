/* Shared packaged view. Data and action ownership stay in the Rust plugins. */
(() => {
  "use strict";
  // The host transport broadcasts messages across windows. This opaque page
  // identity prevents wrong-frame rendering; it does not encrypt that transport.
  const frameBytes = new Uint8Array(16);
  window.crypto.getRandomValues(frameBytes);
  const frameId = Array.from(frameBytes, byte => byte.toString(16).padStart(2, "0")).join("");
  Object.defineProperty(window, "uiKitFrameId", { value: frameId, writable: false, configurable: false });
  const $ = id => document.getElementById(id);
  const tr=(key,fallback,args)=>window.uiKitI18n?.t(key,fallback,args)??fallback;
  const columnLabel=label=>tr(`column.${String(label).toLowerCase().replaceAll(" ","_")}`,label);
  const state = { sequence: 0, project: null, model: null, activeView: "", groupFocus: null, generation: 0, cleanup: null, navigationCleaned: false, section: "", pages: new Map(), selected: null, sort: null, busy: false, refreshPending: false, sectionFocus: null, listeners: [], contextPending: null, contextActionsKey: null, contextApproved: false, requestOp: null, loadFailed: false, sectionTabs: new Map(), statusDelay: 0 };
  const element = (tag, text, className) => { const node = document.createElement(tag); if (text != null) node.textContent = text; if (className) node.className = className; const control={button:"ui-button",input:"ui-input",select:"ui-select",textarea:"ui-textarea",output:"ui-output",table:"ui-table"}[tag]; if(control) node.classList.add(control); return node; };
  const contentScope = () => JSON.stringify([state.project,state.activeView]);
  const pageScope = () => `${state.activeView}\0${state.section}`;
  const queryState = () => { const scope=pageScope();if (!state.pages.has(scope)) state.pages.set(scope, { query: "", facets: {}, page: 0, selected: null, sort: null, scrollTop:0, scrollLeft:0 }); return state.pages.get(scope); };
  // Native notifications own errors/warnings; no persistent banner in panel chrome.
  const notice = (text, warning = false) => {
    if (!text) return;
    if (window.openidea) openidea.postMessage(JSON.stringify({op:"notify",channel:"ui-kit",frameId,sequence:0,message:text,warning}));
    else window.uiKitWidgets?.notify({title:"Plugin view",message:text,tone:"error"});
  };
  function send(op) {
    if (!window.openidea) { notice(tr("shared.openInIdea","Open this view in OpenIDEA to load project resources.")); return; }
    state.contextPending = null;
    state.groupFocus = $("view-tabs").contains(document.activeElement) ? document.activeElement.dataset.viewKey : null;
    state.sectionFocus = $("sections").contains(document.activeElement) ? document.activeElement.dataset.sectionKey : null;
    state.busy = true; $("content").setAttribute("aria-busy", "true");
    if($("request-progress")) $("request-progress").hidden = false;
    // A text-field blur can flush its draft before the user's button click.
    // Keep navigation clickable for field syncs; the following action owns busy state.
    if(!(op.op === "event" && op.event?.kind === "field")) {document.querySelectorAll("#toolbar button, #view-tabs button, #sections button, .field-controls>[data-inline-action]").forEach(b => b.disabled = true);$("view-sections").disabled = true;}
    state.requestOp = op.op;
    openidea.postMessage(JSON.stringify({ targetView: state.activeView || undefined, ...op, channel: "ui-kit", frameId, clientLocale:window.uiKitI18n?.locale, sequence: ++state.sequence, project: state.project }));
    return state.sequence;
  }
  // Optional producer-owned background work. One request may be outstanding;
  // polls stop on navigation, hidden documents and page teardown.
  let backgroundPoll=null,backgroundSequence=0;
  function stopBackgroundPoll(){if(backgroundPoll)clearTimeout(backgroundPoll.timer);backgroundPoll=null;}
  function configureBackgroundPoll(){
    const spec=state.model?.body?.backgroundPoll;
    if(!spec || typeof spec.message!=="object" || Array.isArray(spec.message) || !Number.isSafeInteger(spec.intervalMs) || spec.intervalMs<100 || spec.intervalMs>60000){stopBackgroundPoll();return;}
    const scope=JSON.stringify([state.project,state.activeView,spec]);
    if(backgroundPoll?.scope===scope)return;
    stopBackgroundPoll();const poll={scope,project:state.project,view:state.activeView,pending:null,pendingSince:0,timer:null};backgroundPoll=poll;
    const schedule=()=>{poll.timer=setTimeout(()=>{
      if(backgroundPoll!==poll)return;
      if(poll.pending && performance.now()-poll.pendingSince>=30000)poll.pending=null;
      if(!document.hidden && !poll.pending && !state.busy && window.openidea){
        poll.pending=`${frameId}:${++backgroundSequence}`;poll.pendingSince=performance.now();
        openidea.postMessage(JSON.stringify({op:"opaque-message",message:JSON.stringify({...spec.message,_backgroundPoll:poll.pending}),channel:"ui-kit",frameId,clientLocale:window.uiKitI18n?.locale,sequence:0,targetView:poll.view,project:poll.project}));
      }
      schedule();
    },spec.intervalMs);};schedule();
  }
  window.addEventListener("pagehide",stopBackgroundPoll);
  const event = (data, targetView=state.activeView) => send({ op: "event", event: data, targetView: targetView || undefined });
  const formDrafts = new Map();
  const formScope = () => `${state.project}\0${state.activeView}\0${state.section}`;
  const draftKey = (key, scope=formScope()) => `${scope}\0${key}`;
  function clearFormDrafts(scope) {
    for(const [key,draft] of formDrafts) if(scope == null || draft.scope === scope) { clearTimeout(draft.timer);formDrafts.delete(key); }
  }
  function flushField(key) {
    const mapKey=draftKey(key), draft=formDrafts.get(mapKey);if(!draft) return;
    clearTimeout(draft.timer);draft.timer=null;
    if(draft.scope !== formScope() || state.model?.body.kind !== "form") return;
    if(draft.sentValue === draft.value) return;
    draft.sentValue=draft.value;draft.sequence=event({kind:"field",key,value:draft.value});
  }
  function flushForm() {for(const draft of formDrafts.values()) if(draft.scope === formScope()) flushField(draft.fieldKey);}
  // Model-free invalidation: fetch through this page's correlated bridge rather
  // than accepting state broadcast by another invocation or interrupting a write.
  function drainRefresh() {
    if (!state.refreshPending || state.busy) return;
    flushForm();
    if (state.busy) return;
    state.refreshPending = false;
    send({op:"render"});
  }

  // Native host names and the packaged icon registry are distinct.
  // Only known aliases are translated; an unknown icon keeps its text label.
  const iconAliases = Object.freeze({
    plus:"add", minus:"remove", check:"checkmark", refresh:"update", edit:"pencil",
    more:"more-options", "more-horizontal":"more", list:"table", branch:"branches",
    debug:"bug", diff:"compare", error:"exception", "external-link":"new-window",
    "file-log":"log", "folder-open":"folder", "folder-library":"folder", "folder-resources":"folder",
    "github-pr-closed":"pr-close", "github-pr-merged":"pr-merged", "github-pr-open":"pr-open",
    "github-review":"comment", "github-review-accepted":"checkmark", "github-timeline":"activity",
    "list-filter":"filter", package:"artifacts", rerun:"redo", revert:"undo",
    "review-comment":"comment", star:"star-empty", "file-ignored":"file-config"
  });
  function viewIcon(name) {
    if (typeof name !== "string" || name.length > 128) return null;
    const custom=window.uiKitPluginIcons;
    if(custom && Object.hasOwn(custom,name)) {
      const icon=element("span",null,"view-icon");icon.dataset.uiIconName=name;icon.setAttribute("aria-hidden","true");
      const asset=custom[name];
      for(const [source,variant] of [[asset.icon,asset.iconDark ? "ui-native-light" : ""],[asset.iconDark,"ui-native-dark"]]) {
        if(!source || !/^\.\/plugin-icons\/[a-zA-Z0-9_./-]+\.svg$/.test(source) || source.split("/").includes("..")) continue;
        const image=element("img",null,variant);image.src=source;image.alt="";image.width=16;image.height=16;icon.append(image);
      }
      return icon.childElementCount ? icon : null;
    }
    const native=window.uiKitNativeIcons;
    if(native?.resolve(name)) {const icon=native.create(name,{size:16});icon.classList.add("view-icon");return icon;}
    const registry = window.uiKitIcons;
    if (!registry) return null;
    const resolved = registry.get(name) ? name : Object.hasOwn(iconAliases,name) ? iconAliases[name] : null;
    if (!resolved || !registry.get(resolved)) return null;
    const icon = registry.create(resolved,{size:16});
    icon.classList.add("view-icon");
    return icon;
  }

  function actionButton(button, toolbar = false) {
    if(button.key!=="runtime-runner" || button.label==="Local PHP") button={...button,label:tr(`action.${button.key}`,button.label)};
    const targetView=state.activeView;
    const b = element("button");
    const overflow = button.key === "more-actions" || ["more", "more-horizontal", "more-vertical"].includes(button.icon);
    const label = element("span", button.label, "action-label"); b.append(label); b.type = "button"; b.disabled = !button.enabled;
    const icon = viewIcon(button.icon); if (icon) b.prepend(icon);
    if (toolbar && icon && (overflow || state.model.iconButtons?.includes(button.key) || ["refresh", "reindex", "more-actions", "reset", "expand-all", "collapse-all", "reload-saved", "preferences", "settings", "graph-zoom-in", "graph-zoom-out", "graph-zoom-reset", "graph-clear-selection", "routes:clear"].includes(button.key))) {
      b.classList.add("ui-icon-button"); label.classList.add("ui-visually-hidden"); b.title = button.label;
    }
    if (toolbar && (overflow || ["preferences","settings"].includes(button.key))) b.classList.add("ui-toolbar-end");
    if(toolbar && overflow) b.dataset.uiToolbarOverflow="";
    b.addEventListener("click", () => {if(targetView !== state.activeView)return;flushForm();event({ kind: "button", key: button.key },targetView);}); return b;
  }
  function groupMenu(group, buttons) {
    const targetView=state.activeView,members=(group.items || []).map(key=>buttons.find(b=>b.key === key)).filter(Boolean);
    const id=`toolbar-menu-${group.id}`,title=tr(`toolbar.${group.id}`,group.label);
    const trigger=element("button");trigger.type="button";trigger.setAttribute("popovertarget",id);trigger.setAttribute("aria-haspopup","menu");trigger.title=title;
    trigger.append(element("span",title,"action-label"));
    const icon=viewIcon(group.icon);if(icon)trigger.prepend(icon);
    const chevron=viewIcon("chevron-down");if(chevron)trigger.append(chevron);
    const menu=element("div");menu.id=id;menu.dataset.uiMenu="";menu.setAttribute("popover","manual");menu.setAttribute("aria-label",title);
    for(const b of members) {
      const item=element("button");item.type="button";item.setAttribute("role","menuitem");
      const label=tr(`action.${b.key}`,b.label),content=element("span",null,"ui-menu-label"),glyph=viewIcon(b.icon) || element("span",null,"view-icon");
      glyph.setAttribute("aria-hidden","true");content.append(glyph,document.createTextNode(label));item.append(content);item.title=label;
      item.addEventListener("click",()=>{if(targetView !== state.activeView)return;flushForm();event({ kind: "button", key: b.key },targetView);});
      menu.append(item);
    }
    return [trigger,menu];
  }
  function header() {
    const model = state.model; document.title = model.title;
    document.querySelector("main").setAttribute("aria-label", model.title);
    const focusedView = $("view-tabs").contains(document.activeElement) ? document.activeElement.dataset.viewKey : document.activeElement === document.body ? state.groupFocus : null;
    state.groupFocus = null;
    const viewTabs = Array.isArray(model.viewTabs) ? model.viewTabs : [];
    const panel=document.querySelector("main");
    if(viewTabs.length > 1) {panel.id=`view-panel-${encodeURIComponent(state.activeView)}`;panel.setAttribute("role","tabpanel");panel.setAttribute("aria-labelledby",`view-tab-${encodeURIComponent(state.activeView)}`);}
    else {panel.removeAttribute("id");panel.removeAttribute("role");panel.removeAttribute("aria-labelledby");}
    $("view-tabs").hidden = viewTabs.length < 2;
    // Optional packaged sections show one cluster of tabs at a time behind a compact picker.
    const viewGroups = viewSections(viewTabs), activeSection = viewGroups?.find(section => section.tabs.includes(state.activeView));
    if(activeSection) state.sectionTabs.set(activeSection.id, state.activeView);
    const picker = $("view-sections");
    picker.closest("label").hidden = !activeSection || viewTabs.length < 2;
    picker.disabled = false;
    if(activeSection) {
      picker.replaceChildren(...viewGroups.map(section => {const option = element("option", tr(`viewSection.${section.id}`, section.label));option.value = section.id;return option;}));
      picker.value = activeSection.id;
      picker.closest("label").querySelector("[role=combobox]")?.setAttribute("aria-label", tr("shared.viewGroup", "View group"));
    }
    $("view-tabs").replaceChildren(...viewTabs.filter(tab => !activeSection || activeSection.tabs.includes(tab.id)).map(tab => {
      const button=element("button",tab.label);button.type="button";button.dataset.viewKey=tab.id;
      const packagedTab=Array.isArray(window.uiKitViewTabs) ? window.uiKitViewTabs.find(item=>item.id === tab.id) : null;
      const icon=viewIcon(tab.icon || packagedTab?.icon);if(icon)button.prepend(icon);
      if(Number.isSafeInteger(tab.count) && tab.count >= 0)button.append(element("span",String(tab.count),"tab-count"));
      button.id=`view-tab-${encodeURIComponent(tab.id)}`;button.setAttribute("aria-controls",`view-panel-${encodeURIComponent(tab.id)}`);button.title=tab.label;button.setAttribute("aria-label",Number.isSafeInteger(tab.count) && tab.count >= 0 ? `${tab.label} (${tab.count})` : tab.label);button.setAttribute("role","tab");
      const active=tab.id === state.activeView;button.setAttribute("aria-selected",String(active));button.tabIndex=active ? 0:-1;
      button.addEventListener("click",()=>selectView(tab.id));
      return button;
    }));
    if(focusedView) [...$("view-tabs").children].find(node=>node.dataset.viewKey === focusedView)?.focus({preventScroll:true});
    const focusedSection = $("sections").contains(document.activeElement) ? document.activeElement.dataset.sectionKey : document.activeElement === document.body ? state.sectionFocus : null;
    state.sectionFocus = null;
    const sections = model.toolbar.filter(b => b.enabled && b.key.startsWith("section:"));
    state.section = sections.find(b => b.label.startsWith("●"))?.key || "main";
    $("sections").hidden = sections.length < 2; $("sections").replaceChildren(...sections.map(b => {
      const node = actionButton({ ...b, label: b.label.replace(/^●\s*/, "") });
      node.dataset.sectionKey = b.key;
      if (b.key === state.section) node.setAttribute("aria-current", "page"); return node;
    }));
    if (focusedSection) [...$("sections").children].find(n => n.dataset.sectionKey === focusedSection)?.focus({preventScroll:true});
    // Only producer-declared actions belong in the toolbar. Invalidation keeps
    // local content current without adding a generic Refresh action.
    const inlineButtons=new Set(model.body.kind === "form" ? model.body.fields.flatMap(field=>model.fieldActions?.[field.key]?.button || []) : []);
    const visibleButtons=model.toolbar.filter(b => b.enabled && !b.key.startsWith("section:") && !inlineButtons.has(b.key));
    const groupOf=new Map();
    for(const group of Array.isArray(model.toolbarGroups) ? model.toolbarGroups : [])for(const key of group.items || [])if(!groupOf.has(key))groupOf.set(key,group);
    const emittedGroups=new Set(),toolbarButtons=[];
    for(const b of visibleButtons) {
      const group=groupOf.get(b.key);
      if(!group){toolbarButtons.push(actionButton(b,true));continue;}
      if(emittedGroups.has(group.id))continue;
      emittedGroups.add(group.id);
      toolbarButtons.push(...groupMenu(group,visibleButtons));
    }
    const toolbarOrder=button=>button.hasAttribute("data-ui-toolbar-overflow") ? 2 : Number(button.classList.contains("ui-toolbar-end"));
    toolbarButtons.sort((a,b)=>toolbarOrder(a)-toolbarOrder(b));
    $("toolbar").replaceChildren(...toolbarButtons);
    if(emittedGroups.size)window.uiKitWidgets?.enhance($("toolbar"));
    $("search").value = queryState().query;
    $("search").placeholder = typeof model.body.searchPlaceholder === "string" ? tr(`search.${state.activeView}`,model.body.searchPlaceholder) : tr("shared.searchPlaceholder","Search…");
  }
  function selectView(id) {
    if(id === state.activeView) return;
    stopBackgroundPoll();flushForm();
    if(state.cleanup){state.cleanup();state.cleanup=null;state.navigationCleaned=true;}
    send({op:"select-view",target:id});
  }
  // Sections come from the packaged group (sync-plugin-ui.py) and must cover each tab exactly once.
  function viewSections(viewTabs) {
    const sections = window.uiKitViewSections;
    if(!Array.isArray(sections) || sections.length < 2) return null;
    const ids = viewTabs.map(tab => tab.id), covered = sections.flatMap(section => section.tabs || []);
    if(covered.length !== ids.length || !ids.every(id => covered.includes(id))) return null;
    return sections;
  }
  function rowModel(body) {
    if (body.kind === "resources") {
      const identity = body.columns.findIndex(c => ["Path","Dependency","Package","Artifact","Name"].includes(c));
      return { columns: body.columns, facets: body.facets || [], rows: body.items.map(r => ({ ...r, label: r.label || r.cells[identity < 0 ? 0 : identity] })) };
    }
    if (body.kind === "table") return { columns: body.columns, facets: [], rows: body.rows.map((cells, i) => ({ key: String(i + (body.firstRow || 0)), row: i + (body.firstRow || 0), cells, label: cells[0] })) };
    const hasDetails = body.kind !== "tree" || (body.items || []).some(row=>row.detail || row.method);
    return { columns: hasDetails ? ["Resource", "Details"] : ["Resource"], facets: [], rows: (body.items || []).map(r => ({ ...r, cells: hasDetails ? [r.label, r.detail || ""] : [r.label] })) };
  }
  function select(row, body) {
    queryState().selected = row.key; state.selected = row;
  }
  function showDetails(row, body) {
    const dialog = $("details");
    const title = element("h2", row.label || row.cells[0]); title.id = "resource-details-title";
    const heading = element("div", null, "ui-dialog-header"); heading.append(title);
    const fields = element("dl", null, "resource-facts ui-dialog-body");
    const columns = rowModel(body).columns;
    let hasCode=false;
    const fact = (label,value,isColumn=false) => {
      const detail=element("dd",value || "—"), badge=cellBadge(label,value);
      const preview=badge ? null : window.uiKitCodePreview?.create(label,value);
      if(badge) detail.replaceChildren(badge);
      else if(preview){detail.replaceChildren(preview);hasCode=true;}
      fields.append(element("dt",isColumn?columnLabel(label):label),detail);
    };
    columns.forEach((label,i) => fact(label,row.cells[i],true));
    for (const detail of row.details || []) {
      if (typeof detail.label === "string" && typeof detail.value === "string") fact(detail.label,detail.value);
    }
    if (row.method && !columns.includes("Method")) {
      const method = element("dd"); const badges = methodBadges(row.method);
      if (badges) method.append(badges); else method.textContent = row.method;
      fields.append(element("dt", tr("column.method", "Method")), method);
    }
    const footer = element("div", null, "ui-dialog-footer"); const close = element("button", tr("shared.close","Close"));
    close.addEventListener("click",()=>dialog.close()); footer.append(close);
    dialog.classList.toggle("has-code-preview",hasCode);
    dialog.setAttribute("aria-labelledby",title.id); dialog.replaceChildren(heading, fields, footer);
    if (!dialog.open) dialog.showModal();
  }
  function hasDeclaredRowActions(row) {
    const actions=state.model.actions || [], kind=row.key.split(":")[0];
    return actions.some(action=>Array.isArray(action.keys)) || actions.some(action=>action.enabled !== false && (!action.kind || action.kind === kind));
  }
  function contextToolbarButtons() {
    const allowed=state.model.contextButtons;
    return state.model.toolbar.filter(button=>button.enabled && !button.key.startsWith("section:")
      && !["preferences","settings","back","preferences-back"].includes(button.key)
      && (!Array.isArray(allowed) || allowed.some(id=>button.key === id || (id.endsWith(":") && button.key.startsWith(id)))));
  }
  function rowMenu(row, body, id) {
    const targetView=state.activeView,generation=state.generation;
    const menu = element("div"); menu.id = id; menu.dataset.uiMenu = ""; menu.setAttribute("popover","manual");
    menu.setAttribute("aria-label", tr("shared.actionsFor","Actions for {name}",{name:row.label || row.cells[0]}));
    const labels=new Set();
    const item = (label, action, icon) => {
      if(labels.has(label)) return;labels.add(label);
      const button = element("button"); button.type="button"; button.setAttribute("role","menuitem"); button.title=label;
      const content=element("span",null,"ui-menu-label"), glyph=viewIcon(icon) || element("span",null,"view-icon");
      glyph.setAttribute("aria-hidden","true"); content.append(glyph,document.createTextNode(label)); button.append(content);
      button.addEventListener("click",()=>{if(targetView !== state.activeView || generation !== state.generation)return;state.contextPending=null;action();}); menu.append(button);
    };
    const kind = row.key.split(":")[0];
    const actions=(state.model.actions || []).filter(action=>action.enabled !== false && (!action.kind || action.kind === kind) && (!action.keys || action.keys.includes(row.key)));
    if(row.activationEnabled !== false && !actions.some(action=>action.replacesOpen)) item(row.activationLabel || (row.expandable ? (row.expanded ? tr("shared.collapse","Collapse"):tr("shared.expand","Expand")) : tr("shared.open","Open")),()=>activate(row,body),row.activationIcon || (row.expandable ? (row.expanded ? "chevron-up":"chevron-down") : "file"));
    for (const action of actions) {
      item(action.label,()=>send({op:"command",id:action.id,key:row.key}),action.icon);
    }
    if(!hasDeclaredRowActions(row) && state.contextActionsKey === row.key) {
      for(const button of contextToolbarButtons()) {
        item(button.label,()=>send({op:"item-button",key:row.key,button:button.key}),button.icon);
      }
    } else if(state.contextPending?.key === row.key) {
      const loading=element("button",tr("shared.loadingActions","Loading actions…"));loading.type="button";loading.disabled=true;loading.dataset.contextLoading="";menu.append(loading);
    }
    menu.append(element("hr"));
    item(tr("shared.showDetails","Show Details…"),()=>showDetails(row,body),"info");
    return menu;
  }
  function activate(row, body) { if(row.activationEnabled === false) return; if(body.kind === "tree" && row.expandable){event({kind:row.expanded ? "collapse":"expand",key:row.key});return;} event(body.kind === "table" ? { kind: "table-activate", row: row.row } : { kind: "activate", key: row.key }); }
  function methodBadges(value) {
    const methods = typeof value === "string" ? [...new Set(value.toUpperCase().split("|"))] : [];
    const known = ["GET","HEAD","POST","PUT","PATCH","DELETE","OPTIONS","TRACE","CONNECT","ANY","*"];
    if (!methods.length || methods.some(method => !known.includes(method))) return null;
    const badges = document.createDocumentFragment();
    for (const method of methods) {
      const badge = element("span", method, "ui-badge ui-http-method"); badge.dataset.method = method; badges.append(badge);
    }
    return badges;
  }
  function cellBadge(column, value) {
    if (column === "Method") return methodBadges(value);
    if (!["Type","Kind","Severity"].includes(column) || typeof value !== "string" || !value.trim()) return null;
    const badge = element("span", value, `ui-badge ${column === "Severity" ? "ui-severity-badge" : "ui-type-badge"}`);
    if (column === "Severity") {
      const severity = value.trim().toUpperCase().replace(/\s+\+\s+UNKNOWN$/, "");
      badge.dataset.severity = severity;
      const tone = {CRITICAL:"danger",HIGH:"danger",MODERATE:"warning",MEDIUM:"warning",LOW:"info"}[severity];
      if (tone) badge.classList.add(`ui-badge--${tone}`);
    }
    return badge;
  }
  function renderRows(body) {
    const focusedKey=$("content").dataset.ownerScope === contentScope() && document.activeElement?.matches('tbody tr[data-resource-key]') ? document.activeElement.dataset.resourceKey : null;
    const targetView=state.activeView,generation=state.generation;
    const current=()=>targetView === state.activeView && generation === state.generation;
    const rowEvent=data=>{if(current())return event(data,targetView);};
    const { columns, facets, rows } = rowModel(body); const q = queryState();
    const filters = $("facets"); filters.replaceChildren();
    facets.forEach(column => {
      const label = element("label", null, "ui-field"); label.append(element("span", columnLabel(columns[column]), "ui-visually-hidden")); const select = element("select"); select.setAttribute("aria-label", columnLabel(columns[column]));
      const values = [...new Set(rows.map(r => r.cells[column]).filter(Boolean))].sort();
      const facetLabel=typeof body.facetLabels?.[column] === "string" ? tr(`facet.${state.activeView}.${column}`,body.facetLabels[column]) : `${columnLabel(columns[column])}: ${tr("shared.all","All")}`;
      const all = element("option", `${facetLabel} (${rows.length})`); all.value = ""; select.append(all);
      for (const value of values) { const option = element("option", value); option.value = value; select.append(option); }
      if (!values.includes(q.facets[column])) delete q.facets[column];
      select.value = q.facets[column] || ""; select.addEventListener("change", () => { q.facets[column] = select.value; q.page = 0; renderBody(); });
      label.append(select); filters.append(label);
    });
    const needle = q.query.trim().toLocaleLowerCase();
    $("clear").hidden = !needle && !facets.some(column=>q.facets[column]);
    const compare = (a,b) => String(a.cells[q.sort.column] || "").localeCompare(String(b.cells[q.sort.column] || ""), undefined, { numeric: true }) * (q.sort.descending ? -1 : 1);
    let visible = rows.filter(r => (!needle || [r.method || "", r.searchText || "", ...r.cells].join(" ").toLocaleLowerCase().includes(needle)) && facets.every(c => !q.facets[c] || q.facets[c] === r.cells[c]));
    const treeDepths = new Map();
    if (body.kind === "tree") {
      const byKey = new Map(rows.map(row=>[row.key,row])), children = new Map(), roots=[];
      for(const row of rows) {
        if(row.parent && row.parent !== row.key && byKey.has(row.parent)) {
          if(!children.has(row.parent)) children.set(row.parent,[]);
          children.get(row.parent).push(row);
        } else roots.push(row);
      }
      const siblings = list => q.sort ? [...list].sort(compare) : list;
      const ordered=[], seen=new Set();
      const visit = root => {
        const stack=[{row:root,depth:0}];
        while(stack.length) {
          const {row,depth}=stack.pop(); if(seen.has(row.key)) continue;
          seen.add(row.key);treeDepths.set(row.key,Math.min(depth,32));ordered.push(row);
          for(const child of [...siblings(children.get(row.key)||[])].reverse()) stack.push({row:child,depth:depth+1});
        }
      };
      for(const row of siblings(roots)) visit(row);
      for(const row of rows) if(!seen.has(row.key)) visit(row); // bounded recovery for malformed cycles
      if(needle || facets.some(c=>q.facets[c])) {
        const included=new Set(visible.map(row=>row.key));
        for(const row of visible) {
          let parent=row.parent;const ancestors=new Set();
          while(parent && !ancestors.has(parent)) {ancestors.add(parent);included.add(parent);parent=byKey.get(parent)?.parent;}
        }
        visible=ordered.filter(row=>included.has(row.key));
      } else {
        visible=ordered.filter(row=>{
          let parent=row.parent;const ancestors=new Set();
          while(parent && !ancestors.has(parent)) {ancestors.add(parent);const ancestor=byKey.get(parent);if(ancestor && !ancestor.expanded)return false;parent=ancestor?.parent;}
          return true;
        });
      }
    } else if(q.sort) visible.sort(compare);
    const groupBy=body.kind === "resources" && Number.isInteger(body.groupBy) ? body.groupBy : null;
    if(groupBy !== null) {
      const groupOf=row=>String(row.cells[groupBy] || "");
      const order=[...new Set(visible.map(groupOf))].sort((a,b)=>a.localeCompare(b));
      visible.sort((a,b)=>order.indexOf(groupOf(a))-order.indexOf(groupOf(b)));
    }
    const pages = body.kind === "tree" ? 1 : Math.max(1,Math.ceil(visible.length / 100)); q.page = Math.min(q.page,pages-1);
    const table = element("table",null,body.kind === "resources" ? "resources" : ""); const head = element("thead"); const headings = element("tr");
    const displayed=Array.isArray(body.displayColumns) ? new Set(body.displayColumns) : null;
    columns.forEach((label,column) => { if(displayed && !displayed.has(column)) return; const th = element("th"); const b = element("button", columnLabel(label));
      const style=body.columnStyles?.[column];if(["code","text","muted","locale"].includes(style))th.classList.add(`resource-${style}`);
      if(q.sort?.column === column) {
        b.append(viewIcon(q.sort.descending ? "arrow-12px-down" : "arrow-12px-up"));
      }
      b.addEventListener("click", () => { q.sort = { column, descending: q.sort?.column === column && !q.sort.descending }; renderBody(); });
      th.setAttribute("scope","col"); if(q.sort?.column === column) th.setAttribute("aria-sort",q.sort.descending ? "descending":"ascending"); th.append(b); headings.append(th); });
    if(body.kind === "resources" && body.columnStyles?.length){
      table.classList.add("resource-table--bounded");const group=element("colgroup");
      const weights=columns.map((_,i)=>{const weight=body.columnWeights?.[i];return typeof weight==="number"&&Number.isFinite(weight)&&weight>0&&weight<=100?weight:body.columnStyles[i]==="locale"?0.5:body.columnStyles[i]==="muted"?1:2;}),total=weights.reduce((a,b)=>a+b,0);
      for(const weight of weights){const col=element("col");col.style.width=`${weight/total*100}%`;group.append(col);}table.append(group);
    }
    head.append(headings); table.append(head); const tbody = element("tbody"); let rowIndex=0;
    table.setAttribute("role",body.kind === "tree" ? "treegrid" : "grid");table.setAttribute("aria-label",state.model.title);
    if(body.kind === "tree") {table.classList.add("resource-tree");if(columns.length === 1)table.classList.add("resource-tree--plain");}
    const pageRows=body.kind === "tree" ? visible : visible.slice(q.page*100,(q.page+1)*100);
    const collapsed=q.collapsed || (q.collapsed=new Set()), groupCounts=new Map();
    if(groupBy !== null) for(const row of visible) groupCounts.set(String(row.cells[groupBy] || ""),(groupCounts.get(String(row.cells[groupBy] || ""))||0)+1);
    const shownRows=groupBy === null ? pageRows : pageRows.filter(row=>!collapsed.has(String(row.cells[groupBy] || "")));
    const tabStop=shownRows.find(row=>row.key === q.selected)?.key || shownRows[0]?.key;
    const selectVisible=(row,focus=false,scroll=false)=>{
      if(!current())return;
      select(row,body);
      for(const node of tbody.querySelectorAll('tr')) {const active=node.dataset.resourceKey === row.key;node.setAttribute("aria-selected",String(active));node.tabIndex=active ? 0:-1;if(active && focus){node.focus({preventScroll:true});if(scroll)node.scrollIntoView({block:"nearest",inline:"nearest"});}}
    };
    let lastGroup=null;
    for (const row of pageRows) {
      if(groupBy !== null) {
        const group=String(row.cells[groupBy] || "");
        if(group !== lastGroup) {
          lastGroup=group;
          const open=!collapsed.has(group), header=element("tr",null,"resource-group"), th=element("th");
          th.colSpan=displayed ? displayed.size : columns.length;th.setAttribute("scope","rowgroup");
          const toggle=element("button",null,"resource-group-toggle");toggle.type="button";toggle.setAttribute("aria-expanded",String(open));
          toggle.append(viewIcon(open ? "chevron-12px-down" : "chevron-12px-right"),element("span",group || "—","resource-group-label"),element("span",String(groupCounts.get(group)),"resource-group-count"));
          toggle.addEventListener("click",()=>{if(open)collapsed.add(group);else collapsed.delete(group);renderBody();});
          th.append(toggle);header.append(th);tbody.append(header);
        }
        if(collapsed.has(group)) continue;
      }
      const rowNode = element("tr"); rowNode.tabIndex = row.key === tabStop ? 0:-1; rowNode.setAttribute("aria-selected",String(row.key === q.selected));
      if(body.kind === "tree") {rowNode.setAttribute("aria-level",String((treeDepths.get(row.key)||0)+1));if(row.expandable)rowNode.setAttribute("aria-expanded",String(!!row.expanded));}
      const menuId = `resource-menu-${rowIndex++}`;
      rowNode.dataset.resourceKey = row.key; rowNode.dataset.uiContext = menuId; rowNode.setAttribute("aria-haspopup","menu");
      const contextSelect=e=>{
        if(!current())return;
        selectVisible(row,true);
        if(!state.contextApproved && !hasDeclaredRowActions(row) && (state.model.contextButtons == null || state.model.contextButtons.length > 0)) {
          state.contextActionsKey=null;
          const sequence=rowEvent(body.kind === "table" ? {kind:"table-select",row:row.row} : {kind:"select",key:row.key});
          state.contextPending={key:row.key,sequence,project:state.project,point:{x:e?.clientX || 0,y:e?.clientY || 0}};
        }
        $(menuId)?.remove();const menu=rowMenu(row,body,menuId);$("content").append(menu);window.uiKitWidgets?.enhance(menu);
      };
      rowNode.addEventListener("contextmenu",contextSelect);
      rowNode.addEventListener("keydown",e=>{if(e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) contextSelect(e);});
      row.cells.forEach((cell,i) => { if(displayed && !displayed.has(i)) return; const td = element("td", cell); if (body.kind === "resources") { const badge = groupBy === null ? cellBadge(columns[i],cell) : null; if (badge) td.replaceChildren(badge); else {td.className = "mono";td.title = String(cell);} }
        else if (["tree","list"].includes(body.kind) && i === 1 && row.method) { const badges = methodBadges(row.method); if (badges) td.replaceChildren(badges, document.createTextNode(cell ? ` ${cell}` : "")); else td.textContent = `${row.method} ${cell}`; }
        const style=groupBy !== null && i > 0 ? "muted" : body.columnStyles?.[i];
        if(groupBy !== null && i === 0) td.classList.add("resource-grouped");
        if(["code","text","muted","locale"].includes(style)){td.classList.remove("mono");td.classList.add(`resource-${style}`);td.title=String(cell);const value=element("span",null,"resource-cell-value");value.append(...td.childNodes);td.append(value);}
        if(i === 0 && body.kind === "resources" && body.showRowIcons === true){
          const line=element("span",null,"resource-identity"),label=element("span",cell,"resource-cell-label"),icon=viewIcon(row.icon);
          if(icon)line.append(icon);line.append(label);td.replaceChildren(line);
        }
        if(groupBy === null && ["Kind","Type","Method","Severity","Ecosystem"].includes(columns[i]) && /^[\p{L}\p{N}_-]{1,24}$/u.test(String(cell))) td.classList.add("category");
        if (i === 0 && body.kind === "list") {
          const icon = viewIcon(row.icon); if (icon) td.prepend(icon);
        }
        if (i === 0 && body.kind === "tree") {
          const line=element("div",null,"tree-cell");line.style.paddingLeft=`${(treeDepths.get(row.key)||0)*20}px`;
          const label=element("span",cell,"tree-label");label.title=cell;
          let toggle=element("span",null,"tree-node-slot");toggle.setAttribute("aria-hidden","true");
          if(row.expandable) {
            toggle=element("button",null,"tree-toggle");toggle.tabIndex=-1;toggle.append(viewIcon(row.expanded ? "chevron-down" : "chevron-right"));
            toggle.setAttribute("aria-label",row.expanded ? tr("shared.collapseItem","Collapse {name}",{name:row.label}) : tr("shared.expandItem","Expand {name}",{name:row.label}));
            toggle.addEventListener("click",e=>{e.stopPropagation();rowEvent({kind:row.expanded ? "collapse":"expand",key:row.key});});
          }
          const icon=viewIcon(row.icon)||element("span",null,"view-icon");icon.setAttribute("aria-hidden","true");
          line.append(toggle,icon,label);td.replaceChildren(line);
        }
        if(body.editable) {
          const original=td.cloneNode(true);td.contentEditable = "true";td.setAttribute("aria-label",columnLabel(columns[i]));
          td.addEventListener("keydown",e => {
            if(e.key === "Enter") {e.preventDefault();td.blur();}
            else if(e.key === "Escape") {e.preventDefault();td.replaceChildren(...original.cloneNode(true).childNodes);td.blur();}
          });
          td.addEventListener("blur",() => { if(td.textContent !== String(cell)) rowEvent({kind:"cell",row:row.row,column:i,value:td.textContent}); });
        }
        rowNode.append(td);
      });
      rowNode.addEventListener("click", () => { if(!current())return;selectVisible(row);rowEvent(body.kind === "table" ? {kind:"table-select",row:row.row} : {kind:"select",key:row.key}); });
      rowNode.addEventListener("dblclick",() => {if(current())activate(row,body);});
      rowNode.addEventListener("keydown",e => {
        if(e.target !== rowNode || !current() || e.ctrlKey || e.metaKey || e.altKey) return;
        if(e.key === "Enter") {e.preventDefault();activate(row,body);return;}
        if(e.key === " ") {e.preventDefault();selectVisible(row);rowEvent(body.kind === "table" ? {kind:"table-select",row:row.row}:{kind:"select",key:row.key});return;}
        const at=shownRows.indexOf(row);
        let next=e.key === "ArrowDown" ? shownRows[Math.min(at+1,shownRows.length-1)] : e.key === "ArrowUp" ? shownRows[Math.max(0,at-1)] : e.key === "Home" ? shownRows[0] : e.key === "End" ? shownRows.at(-1) : null;
        if(body.kind === "tree" && ["ArrowLeft","ArrowRight"].includes(e.key)) {
          e.preventDefault();
          if(e.key === "ArrowLeft") {
            if(row.expandable && row.expanded) {rowEvent({kind:"collapse",key:row.key});return;}
            next=shownRows.find(candidate=>candidate.key === row.parent);
          } else {
            if(row.expandable && !row.expanded) {rowEvent({kind:"expand",key:row.key});return;}
            next=shownRows.find(candidate=>candidate.parent === row.key);
          }
        }
        if(next) {e.preventDefault();if(next !== row){selectVisible(next,true,true);rowEvent(body.kind === "table" ? {kind:"table-select",row:next.row}:{kind:"select",key:next.key});}}
      });
      tbody.append(rowNode);
    }
    table.append(tbody); const content = $("content");
    if(visible.length) content.replaceChildren(table);
    else if(rows.length) content.replaceChildren(emptyState("no-match",tr("shared.noMatching","No matching resources"),tr("shared.trySearch","Try a different search or clear the filters."),tr("shared.resetFilters","Reset filters"),()=>$("clear").click()));
    else {
      const emptyAction=state.model.toolbar.find(button=>button.key === body.emptyAction && button.enabled);
      content.replaceChildren(emptyState("empty",typeof body.emptyTitle === "string" ? body.emptyTitle : tr("shared.noResources","No resources found"),typeof body.emptyText === "string" ? body.emptyText : tr("shared.resourcesAppear","Resources appear here when available."),emptyAction?.label,emptyAction && (()=>rowEvent({kind:"button",key:emptyAction.key}))));
    }
    if (pages > 1) { const pager=element("div",null,"pager"); const prev=element("button",tr("shared.previous","Previous")), next=element("button",tr("shared.next","Next")); prev.hidden=q.page===0;next.hidden=q.page===pages-1;prev.addEventListener("click",()=>{q.page--;renderBody();});next.addEventListener("click",()=>{q.page++;renderBody();});pager.append(prev,element("span",`${q.page+1} / ${pages}`),next);content.append(pager); }
    const selected = rows.find(r=>r.key===q.selected); if(selected) select(selected,body); else {$("details").close();q.selected=null;state.selected=null;}
    if(focusedKey) {
      const nodes=[...tbody.querySelectorAll('tr')],next=nodes.find(node=>node.dataset.resourceKey === focusedKey) || nodes.find(node=>node.dataset.resourceKey === q.selected) || nodes.find(node=>node.tabIndex === 0);
      next?.focus({preventScroll:true});
    }
    setStatus(tr("shared.resourceCount","{visible} of {total} resources",body.kind === "tree" ? {visible:visible.filter(row=>!row.parent).length,total:rows.filter(row=>!row.parent).length} : {visible:visible.length,total:rows.length})+(body.totalRows>rows.length?" "+tr("shared.availableCount","({count} available)",{count:body.totalRows}):"")+(state.model.status?` · ${state.model.status}`:""),state.statusDelay);
  }
  function inlineActionState(model) {
    const keys=Object.values(model?.fieldActions || {}).map(action=>action.button);
    return JSON.stringify([model?.fieldActions,model?.toolbar.filter(button=>keys.includes(button.key))]);
  }
  function restoreInlineActions() {
    document.querySelectorAll('.field-controls>[data-inline-action]').forEach(button=>button.disabled=!state.model?.toolbar.some(action=>action.key === button.dataset.inlineAction && action.enabled));
  }
  function renderForm(body) {
    const targetView=state.activeView,generation=state.generation;
    const current=()=>targetView === state.activeView && generation === state.generation;
    const active=document.activeElement;
    const focus=$("content").dataset.ownerScope === contentScope() && active?.dataset?.fieldKey ? {key:active.dataset.fieldKey,start:active.selectionStart,end:active.selectionEnd,direction:active.selectionDirection} : null;
    const form = element("div",null,"form");
    for(const field of body.fields) {
      const inline=state.model.fieldActions?.[field.key];
      const inlineButton=inline && state.model.toolbar.find(button=>button.key === inline.button && button.enabled);
      const value=field.value,label=element(inlineButton ? "div":"label",null,"ui-field");
      const caption=element(inlineButton ? "label":"span",field.label,"field-label");label.append(caption);let input;
      if(value.kind === "label" || value.kind === "readonly-text") { input=element("output",value.kind === "label" ? value.data : value.data?.text); }
      else if(value.kind === "choice") {input=element("select");for(const text of value.options) {const o=element("option",text);o.value=text;input.append(o);}input.value=value.text;}
      else if(value.kind === "multiline") {input=element("textarea");input.value=value.text;}
      else if(value.kind === "checkbox") {input=element("input");input.type="checkbox";input.className="ui-choice";input.checked=value.checked;label.className="checkbox ui-check";}
      else if(value.kind === "text") {input=element("input");input.type="text";input.value=value.text;}
      else {input=element("output",tr("shared.unsupportedField","Unsupported field type"));}
      input.setAttribute("aria-label",field.label);input.dataset.fieldKey=field.key;
      if(value.kind === "text" || value.kind === "multiline") {
        const draft=formDrafts.get(draftKey(field.key));if(draft?.scope === formScope()) input.value=draft.value;
        input.addEventListener("input",()=>{
          if(!current())return;
          const previous=formDrafts.get(draftKey(field.key));clearTimeout(previous?.timer);
          const next={...previous,fieldKey:field.key,scope:formScope(),value:input.value,timer:null};
          next.timer=setTimeout(()=>{if(next.scope === formScope()) flushField(field.key);},250);formDrafts.set(draftKey(field.key),next);
        });
        input.addEventListener("change",()=>{if(current())flushField(field.key);});
        input.addEventListener("keydown",e=>{if(current() && e.key === "Enter" && value.kind === "text") flushField(field.key);});
      } else if(value.kind === "checkbox" || value.kind === "choice") input.addEventListener("change",()=>{if(!current())return;flushForm();event({kind:"field",key:field.key,value:value.kind === "checkbox" ? String(input.checked):input.value},targetView);});
      if(inlineButton) {
        input.id=`form-field-${form.childElementCount}`;caption.htmlFor=input.id;
        const controls=element("div",null,"field-controls"),button=actionButton(inlineButton);
        button.dataset.inlineAction=inlineButton.key;
        if(inline.primary) button.classList.add("ui-button--primary");
        controls.append(input,button);label.append(controls);
      } else if(value.kind === "checkbox") label.prepend(input); else label.append(input);
      form.append(label);
    }
    $("content").replaceChildren(form);$("details").close();
    window.uiKitControls?.enhance(form);
    if(focus) {const next=Array.from(form.querySelectorAll("input,textarea,button[role=combobox]")).find(n=>n.dataset.fieldKey === focus.key);if(next){next.focus({preventScroll:true});if(focus.start != null && next.setSelectionRange) next.setSelectionRange(Math.min(focus.start,next.value.length),Math.min(focus.end,next.value.length),focus.direction || "none");}}
  }
  const htmlDrafts = new Map();
  function captureHtmlDraft() {
    if(state.model?.body.kind !== "html") return;
    const shadow=$("content").querySelector(".legacy")?.shadowRoot;if(!shadow) return;
    const fields=[...shadow.querySelectorAll('textarea,input:not([type=hidden]),select')];
    if(!fields.some(field=>!field.readOnly && !field.disabled)){htmlDrafts.delete(formScope());return;}
    htmlDrafts.set(formScope(),{html:state.model.body.html,top:$("content").scrollTop,left:$("content").scrollLeft,
      fields:fields.map((field,index)=>({index,id:field.id,tag:field.tagName,type:field.type,value:field.value,checked:field.checked,start:field.selectionStart,end:field.selectionEnd,top:field.scrollTop,left:field.scrollLeft}))});
  }
  function restoreHtmlDraft(body, shadow) {
    const draft=htmlDrafts.get(formScope());if(!draft) return;
    if(draft.html !== body.html){htmlDrafts.delete(formScope());return;}
    const fields=[...shadow.querySelectorAll('textarea,input:not([type=hidden]),select')];
    for(const value of draft.fields) {
      const field=fields[value.index];if(!field || field.readOnly || field.disabled || field.id !== value.id || field.tagName !== value.tag || field.type !== value.type) continue;
      field.value=value.value;if(typeof value.checked === "boolean")field.checked=value.checked;
      if(value.start != null && field.setSelectionRange)field.setSelectionRange(value.start,value.end);
      field.scrollTop=value.top;field.scrollLeft=value.left;field.dispatchEvent(new Event("input",{bubbles:true}));
    }
    $("content").scrollTop=draft.top;$("content").scrollLeft=draft.left;
  }
  function renderHtml(body) {
    const host=element("div",null,"legacy");const shadow=host.attachShadow({mode:"open"});
    const parsed=new DOMParser().parseFromString(body.html,"text/html");parsed.querySelectorAll("script").forEach(s=>s.remove());
    for(const attribute of parsed.body.attributes) host.setAttribute(attribute.name,attribute.value);
    host.classList.add("legacy");
    host.dataset.uiManaged = "legacy";
    // Retain standalone fallback/layout CSS without letting its type selectors
    // and local theme defaults override the managed kit controls/theme.
    for(const style of parsed.querySelectorAll("style")) {style.textContent=`@layer ui-legacy {${style.textContent.replace(/:root|\bhtml\b|\bbody\b/g,":host")}}`;}
    shadow.append(...Array.from(parsed.head.children),...Array.from(parsed.body.childNodes));$("content").replaceChildren(host);$("details").close();
    if(shadow.querySelector("[data-command-workspace], [data-generator-workspace], [data-database-workspace]"))host.classList.add("command-workspace-host");
    // Syntax overlays have a matching pre/textarea geometry and must stay raw.
    shadow.querySelector("#src")?.setAttribute("data-ui-raw", "");
    window.uiKitControls?.enhance(shadow);
    const targetView=state.activeView,generation=state.generation;
    const bridge={project:state.project,postMessage:value=>{if(state.generation === generation) event({kind:"message",value},targetView);},onMessage:callback=>state.listeners.push(message=>{if(state.generation === generation) callback(message);}),saveFile:options=>window.openidea.saveFile(options),openFile:options=>window.openidea.openFile(options)};
    if(host.hasAttribute("data-runtime-controls") || shadow.querySelector("[data-runtime-controls]") || shadow.querySelector("[data-command-workspace], [data-generator-workspace], [data-managed-processes], [data-database-workspace]")) {
      const runtimeBridge={...bridge,postMessage:value=>{
        if(state.generation !== generation || !window.openidea) return;
        openidea.postMessage(JSON.stringify({op:"opaque-message",message:value,channel:"ui-kit",frameId,clientLocale:window.uiKitI18n?.locale,sequence:0,targetView,project:state.project}));
      }};
      const adapter=shadow.querySelector("[data-generator-workspace]") ? window.uiKitGeneratorWorkspace : shadow.querySelector("[data-database-workspace]") ? window.uiKitDatabaseWorkspace : shadow.querySelector("[data-managed-processes]") ? window.uiKitManagedProcesses : shadow.querySelector("[data-command-workspace]") ? window.uiKitCommandWorkspace : window.uiKitRuntimeControls;
      state.cleanup=adapter?.mount(shadow,runtimeBridge) || null;
    }
    else if(shadow.querySelector("svg .n[data-id]")) shadow.querySelectorAll(".n[data-id]").forEach(node=>{const open=()=>bridge.postMessage(node.dataset.id);node.addEventListener("click",open);node.addEventListener("keydown",e=>{if(e.key === "Enter" || e.key === " "){e.preventDefault();open();}});});
    else if(shadow.querySelector("#src")) window.uiKitLegacy?.kube(shadow,bridge);
    else if(shadow.querySelector(".file[data-new-path]")) window.uiKitLegacy?.review(shadow,bridge);
    restoreHtmlDraft(body,shadow);
    const localeCleanup=window.uiKitI18n?.observe(shadow),adapterCleanup=state.cleanup;
    state.cleanup=()=>{adapterCleanup?.();localeCleanup?.();};
    state.listeners.push(message=>{if(message === "end") $("content").scrollTop=$("content").scrollHeight;});
  }
  // Shared empty state: empty, no-match (reset), error (retry) and denied (permission + retry).
  function emptyState(variant,title,text,actionLabel,action) {
    const empty=element("div",null,"empty ui-empty");empty.dataset.state=variant;
    const heading=element("strong",title),iconName={error:"error",denied:"lock"}[variant],icon=iconName ? viewIcon(iconName) : null;
    if(icon){icon.setAttribute("aria-hidden","true");heading.prepend(icon);heading.classList.add("empty-title");}
    empty.append(heading,element("p",text));
    if(action){const button=element("button",actionLabel);button.type="button";button.addEventListener("click",action);empty.append(button);}
    return empty;
  }
  function renderLoadFailure(message) {
    state.cleanup?.();state.cleanup=null;state.generation++;state.listeners=[];state.loadFailed=true;
    $("filters").hidden=true;$("details").close();setStatus("");
    const retry=()=>send({op:"render"}),permission=typeof message.denied === "string" ? message.denied.trim() : null;
    const capability=permission && /^[a-z][a-z0-9-]*(?:[.:][a-z0-9_-]+)+$/i.test(permission),plugin=String(message.plugin || state.model?.plugin || "");
    const empty=permission
      ? emptyState("denied",tr("shared.permissionNeeded","Permission needed"),capability ? tr("shared.deniedCapability","This view needs the {permission} permission. Allow it in Settings › Plugins › {plugin}, then retry.",{permission,plugin}) : `${permission}\n${tr("shared.deniedHint","Review this plugin's permissions in Settings › Plugins › {plugin}, then retry.",{plugin})}`,tr("shared.retry","Retry"),retry)
      : emptyState("error",tr("shared.loadFailed","Couldn't load this view"),String(message.error || ""),tr("shared.retry","Retry"),retry);
    $("content").replaceChildren(empty);$("content").dataset.ownerScope=contentScope();
  }
  let statusTimer=0;
  function setStatus(text,delay=0) {
    clearTimeout(statusTimer);
    if(delay) statusTimer=setTimeout(()=>{$("status").textContent=text;},delay);
    else $("status").textContent=text;
  }
  function renderBody() {
    state.cleanup?.();state.cleanup=null;state.loadFailed=false;
    const body=state.model.body;state.generation++;state.listeners=[];$("filters").hidden=["form","html","message"].includes(body.kind);if(["form","html","message"].includes(body.kind))setStatus(state.model.status || "");
    if(body.kind === "form") renderForm(body);
    else if(body.kind === "html") renderHtml(body);
    else if(body.kind === "message") {$("content").replaceChildren(element("div",body.text,"empty ui-empty"));$("details").close();}
    else renderRows(body);
    $("content").dataset.ownerScope=contentScope();
  }
  // Section navigation follows the kit tab strip without adding a second window header.
  $("sections").addEventListener("keydown", e => {
    const items = [...$("sections").querySelectorAll("button:not(:disabled)")];
    const index = items.indexOf(e.target); if (index < 0) return;
    const next = e.key === "ArrowRight" ? items[(index + 1) % items.length] : e.key === "ArrowLeft" ? items[(index + items.length - 1) % items.length] : e.key === "Home" ? items[0] : e.key === "End" ? items.at(-1) : null;
    if (next) {e.preventDefault(); next.focus(); next.scrollIntoView({block:"nearest",inline:"nearest"});}
  });
  $("view-sections").addEventListener("change",()=>{
    const section=window.uiKitViewSections?.find(item=>item.id === $("view-sections").value);if(!section)return;
    const remembered=state.sectionTabs.get(section.id);
    selectView(section.tabs.includes(remembered) ? remembered : section.tabs[0]);
  });
  $("details").addEventListener("close",()=>{const row=[...document.querySelectorAll("tbody tr")].find(n=>n.dataset.resourceKey === state.selected?.key);row?.focus({preventScroll:true});});
  $("search").addEventListener("input",()=>{const q=queryState();q.query=$("search").value;q.page=0;state.statusDelay=400;try{renderBody();}finally{state.statusDelay=0;}});
  $("clear").addEventListener("click",()=>{const q=queryState();q.query="";q.facets={};q.page=0;$("search").value="";renderBody();});
  window.openidea?.onMessage(text=>{
    let message;try{message=JSON.parse(text);}catch{state.listeners.forEach(callback=>callback(text));return;}
    if(message.channel === "ui-kit-view-message") {
      let schedulerReply=false;
      try{const reply=JSON.parse(message.message);schedulerReply=reply && typeof reply === "object" && Object.hasOwn(reply,"_backgroundPoll");}catch{}
      if(message.frameId===frameId && message.targetView===state.activeView && backgroundPoll?.pending){
        try{const reply=JSON.parse(message.message);if(reply._backgroundPoll===backgroundPoll.pending)backgroundPoll.pending=null;}catch{}
      }
      // Scheduler acknowledgments cannot consume an embedded form's request.
      if(schedulerReply)return;
      if((message.frameId == null || message.frameId === frameId) && message.targetView === state.activeView && typeof message.message === "string") state.listeners.forEach(callback=>callback(message.message));
      return;
    }
    if(message.channel === "ui-kit-refresh") {if(message.targetView != null && message.targetView !== state.activeView) return;state.refreshPending=true;drainRefresh();return;}
    if(message.channel !== "ui-kit") {state.listeners.forEach(callback=>callback(text));return;}
    if(message.frameId !== frameId) return;
    if(message.sequence !== state.sequence) return;
    const context=state.contextPending?.sequence === message.sequence ? state.contextPending : null;
    state.contextPending=null;
    const requestOp=state.requestOp;state.requestOp=null;
    document.querySelectorAll('[data-context-loading]').forEach(node=>node.remove());
    state.busy=false;$("content").setAttribute("aria-busy","false");
    if($("request-progress")) $("request-progress").hidden = true;
    if(state.refreshPending) queueMicrotask(drainRefresh);
    if(message.notified) return;
    if(message.error && requestOp === "render") {if(state.model){header();restoreInlineActions();}renderLoadFailure(message);return;}
    if(message.error) {if(state.model) {header();restoreInlineActions();if(state.navigationCleaned){state.navigationCleaned=false;renderBody();}if(state.selected) select(state.selected,state.model.body);}return;}
    const previousLanguage=window.uiKitI18n?.locale;
    window.uiKitI18n?.setLocale(message.model.uiLanguage || "auto");
    const languageChanged=previousLanguage!==window.uiKitI18n?.locale;
    if(languageChanged)captureHtmlDraft();
    if(Array.isArray(message.model.viewTabs)) message.model.viewTabs=message.model.viewTabs.map(tab=>({...tab,label:tr(`view.${tab.id}`,tab.label)}));
    const nextView=typeof message.model.activeView === "string" ? message.model.activeView : "";
    const viewChanged=state.activeView !== nextView;
    // Producer scheduling metadata does not change controls or their drafts.
    const presentation=body=>{if(!body)return body;const {backgroundPoll,...content}=body;return content;};
    const sameBody=!languageChanged && !viewChanged && state.project === message.project && JSON.stringify(presentation(state.model?.body)) === JSON.stringify(presentation(message.model.body));
    const previousSection=state.section,oldScope=formScope();
    if(viewChanged && state.project === message.project) {captureHtmlDraft();const previous=queryState();previous.scrollTop=$("content").scrollTop;previous.scrollLeft=$("content").scrollLeft;}
    if(state.project !== message.project) {state.pages.clear();state.selected=null;clearFormDrafts();htmlDrafts.clear();}
    if(viewChanged) {
      state.selected=null;state.contextActionsKey=null;state.contextPending=null;
      document.querySelectorAll('[data-ui-menu]').forEach(menu=>{if(menu.matches(':popover-open'))menu.hidePopover();});
      $("details").close();
      for(const draft of formDrafts.values()) if(draft.scope === oldScope) {clearTimeout(draft.timer);draft.timer=null;}
    }
    let formRefresh=message.model.body.kind === "form" && inlineActionState(state.model) !== inlineActionState(message.model);
    state.project=message.project;state.activeView=nextView;state.model=message.model;state.navigationCleaned=false;notice("");header();configureBackgroundPoll();
    if(!viewChanged && (oldScope !== formScope() || state.model.body.kind !== "form")) clearFormDrafts(oldScope);
    if(!viewChanged && oldScope !== formScope()) htmlDrafts.delete(oldScope);
    const htmlDraft=htmlDrafts.get(formScope());
    if(htmlDraft && (state.model.body.kind !== "html" || htmlDraft.html !== state.model.body.html)) htmlDrafts.delete(formScope());
    if(state.model.body.kind !== "form") clearFormDrafts(formScope());
    if(state.model.body.kind === "form") for(const [key,draft] of formDrafts) {
      if(draft.scope !== formScope()) continue;
      const field=state.model.body.fields.find(f=>f.key === draft.fieldKey && ["text","multiline"].includes(f.value.kind));
      if(!field || (draft.sequence <= message.sequence && draft.sentValue === draft.value)) {if(field && field.value.text !== draft.value) formRefresh=true;clearTimeout(draft.timer);formDrafts.delete(key);}
    }
    if(!sameBody || previousSection !== state.section || formRefresh || state.loadFailed) renderBody();
    else {if(state.model.status)setStatus(state.model.status);if(state.selected) select(state.selected,state.model.body);}
    if(viewChanged) {const current=queryState();$("content").scrollTop=current.scrollTop || 0;$("content").scrollLeft=current.scrollLeft || 0;}
    restoreInlineActions();
    if(context && context.project === state.project) {
      const row=[...document.querySelectorAll('tbody tr')].find(node=>node.dataset.resourceKey === context.key);
      if(row) {
        state.contextActionsKey=context.key;state.contextApproved=true;
        try {row.dispatchEvent(new MouseEvent("contextmenu",{bubbles:true,cancelable:true,clientX:context.point.x,clientY:context.point.y}));}
        finally {state.contextApproved=false;}
      }
    }
  });
  document.addEventListener("keydown",event=>{if(event.key === "Escape") state.contextPending=null;},true);
  document.addEventListener("pointerdown",event=>{if(!event.target.closest('[data-ui-menu]')) state.contextPending=null;},true);
  window.addEventListener("ui-kit-notification",e=>{if(typeof e.detail?.message === "string") notice(e.detail.message,e.detail.warning === true);});
  window.addEventListener("error",e=>{
    // The browser defers undelivered resize observations to a later frame.
    // This synthetic event is not a failed plugin operation. Leave default
    // developer logging intact; actual exceptions, even with this text, report.
    const resizeDiagnostic = ["ResizeObserver loop completed with undelivered notifications.", "ResizeObserver loop limit exceeded"].includes(e.message)
      && (e.error == null || e.error === e.message) && !e.lineno && !e.colno;
    if (!resizeDiagnostic) notice(e.message || "The view could not be rendered.");
  });
  send({op:"render"});
})();
