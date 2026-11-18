/* Laravel Database and Schema pages. All data comes from the Rust handler; nothing here runs SQL. */
(() => {
  'use strict';
  const SVG = 'http://www.w3.org/2000/svg';
  const diagramViews = new Map();
  const STATUS = {
    'pending': ['statusPending', 'Pending migration'],
    'only-migrations': ['statusOnlyMigrations', 'Only in migrations'],
    'only-database': ['statusOnlyDatabase', 'Only in the database'],
    'differs': ['statusDiffers', 'Differs']
  };
  const BOX = {width: 230, head: 26, row: 18, rows: 14, gapX: 90, gapY: 28};

  window.uiKitDatabaseWorkspace = {mount(root, bridge) {
    const tr = (key, fallback) => window.uiKitI18n?.t(`database.${key}`, fallback) ?? fallback;
    const format = (key, fallback, args) => tr(key, fallback).replace(/\{(\w+)\}/g, (token, name) => name in args ? String(args[name]) : token);
    const $ = id => root.querySelector(`#${id}`);
    const el = (tag, text, className) => {
      const node = document.createElement(tag);
      if (text != null) node.textContent = text;
      if (className) node.className = className;
      return node;
    };
    const svg = (tag, attributes, ...children) => {
      const node = document.createElementNS(SVG, tag);
      for (const [name, value] of Object.entries(attributes || {})) node.setAttribute(name, value);
      node.append(...children.filter(child => child != null));
      return node;
    };
    const hasDiagram = !!$('dbw-diagram');
    const projectKey = bridge.project || '';
    let disposed = false;
    let queue = [];
    let state = null;
    let rowsRequest = null;
    let rowsReply = null;
    let renderDiagram = null;

    const setError = message => { $('dbw-error').textContent = message || ''; };
    const busy = () => queue.length > 0;
    function refreshControls() {
      const disabled = busy();
      for (const id of ['dbw-compare', 'dbw-rows-load', 'dbw-refresh', 'dbw-table-refresh']) {
        const button = $(id);
        if (button) button.disabled = disabled;
      }
      pager();
      $('dbw-status').textContent = disabled ? tr('working', 'Working…') : '';
    }
    function send(request) {
      if (disposed || busy()) return false;
      setError('');
      queue.push(request.op);
      refreshControls();
      bridge.postMessage(JSON.stringify(request));
      return true;
    }

    const open = place => place && send({op: 'open', path: place.path, line: place.line});
    function openLink(label, place) {
      if (!place) return el('span', label);
      const button = el('button', label, 'ui-link dbw-link');
      button.type = 'button';
      button.title = tr('openMigration', 'Open the migration');
      button.addEventListener('click', () => open(place));
      return button;
    }
    function grid(headings, rows) {
      const table = el('table', null, 'ui-table dbw-table');
      const head = el('tr');
      for (const heading of headings) {
        const th = el('th');
        th.append(heading instanceof Node ? heading : document.createTextNode(heading));
        head.append(th);
      }
      const thead = el('thead');
      thead.append(head);
      table.append(thead);
      const body = el('tbody');
      for (const cells of rows) {
        const row = el('tr');
        for (const cell of cells) {
          const td = el('td');
          td.append(cell instanceof Node ? cell : document.createTextNode(cell ?? ''));
          row.append(td);
        }
        body.append(row);
      }
      table.append(body);
      return table;
    }
    const empty = text => el('div', text, 'ui-empty dbw-empty');
    const keyLabel = key => ({pk: 'PK', fk: 'FK', uk: 'UK'})[key] || '';

    /* ---------- Tables ---------- */
    function describeIndex(index) {
      return `${index.kind}(${index.columns.join(', ')})`;
    }
    const expanded = new Set();
    function icon(name) {
      const node = el('span', null, 'dbw-icon');
      node.dataset.uiIcon = name;
      node.setAttribute('aria-hidden', '1');
      return node;
    }
    function treeRows(table, query) {
      const key = `t:${table.name}`;
      const columnsKey = `${key}:columns`;
      const indexesKey = `${key}:indexes`;
      const foreignsKey = `${key}:foreigns`;
      const count = table.columns.length === 1 ? format('columnCountOne', '{count} column', {count: 1}) : format('columnCount', '{count} columns', {count: table.columns.length});
      const details = [table.models.join(', '), count].filter(Boolean).join(' · ');
      const rows = [{key, depth: 0, icon: 'table', label: table.name, details, children: true, open: table.open}];
      if (!expanded.has(key)) return rows;
      rows.push({key: columnsKey, depth: 1, icon: 'folder', label: `${tr('columns', 'Columns')} (${table.columns.length})`, children: table.columns.length > 0});
      if (expanded.has(columnsKey)) {
        for (const column of table.columns) rows.push({key: `${columnsKey}:${column.name}`, depth: 2, badge: keyLabel(column.key), keyKind: column.key, label: column.name, details: column.kind, hit: !!query && column.name.toLowerCase().includes(query)});
      }
      if (table.indexes.length) {
        rows.push({key: indexesKey, depth: 1, icon: 'folder', label: `${tr('indexes', 'Indexes')} (${table.indexes.length})`, children: true});
        if (expanded.has(indexesKey)) for (const index of table.indexes) rows.push({key: `${indexesKey}:${describeIndex(index)}`, depth: 2, label: describeIndex(index), details: index.kind});
      }
      if (table.foreigns.length) {
        rows.push({key: foreignsKey, depth: 1, icon: 'folder', label: `${tr('foreignKeys', 'Foreign keys')} (${table.foreigns.length})`, children: true});
        if (expanded.has(foreignsKey)) for (const foreign of table.foreigns) rows.push({key: `${foreignsKey}:${foreign.column}`, depth: 2, badge: 'FK', keyKind: 'fk', label: foreign.column, details: `${foreign.on ?? '?'}.${foreign.references}`});
      }
      for (const row of rows) {
        row.open = table.open;
        if (!row.children || row.depth === 0) row.copy = row.depth === 0 ? table.name : row.label.replace(/ \(\d+\)$/, '');
      }
      return rows;
    }
    function copyText(text) {
      const done = () => { $('dbw-status').textContent = tr('copied', 'Copied'); setTimeout(() => { if (!busy()) $('dbw-status').textContent = ''; }, 1200); };
      try { navigator.clipboard.writeText(text).then(done, () => setError(tr('copyFailed', 'Copy is not available here.'))); } catch { setError(tr('copyFailed', 'Copy is not available here.')); }
    }
    let menu = null;
    let menuReturn = null;
    function closeMenu(refocus) {
      menu?.remove();
      menu = null;
      if (refocus) menuReturn?.focus();
    }
    function showMenu(x, y, row, from) {
      closeMenu();
      menuReturn = from;
      const items = [];
      if (row.children) items.push([expanded.has(row.key) ? tr('collapse', 'Collapse') : tr('expand', 'Expand'), expanded.has(row.key) ? 'chevron-up' : 'chevron-down', () => toggle(row.key)]);
      if (row.open) items.push([tr('goToMigration', 'Go to Migration'), 'file-php', () => open(row.open)]);
      if (row.copy) items.push([tr('copyName', 'Copy Name'), 'copy', () => copyText(row.copy)]);
      items.push([tr('expandAll', 'Expand All'), 'expand-all', () => setAll(true)], [tr('collapseAll', 'Collapse All'), 'collapse-all', () => setAll(false)]);
      menu = el('ul', null, 'dbw-menu');
      menu.setAttribute('role', 'menu');
      for (const [label, iconName, run] of items) {
        const item = el('li', null, 'dbw-menu-item');
        item.setAttribute('role', 'menuitem');
        item.tabIndex = -1;
        item.append(icon(iconName), el('span', label));
        item.addEventListener('click', () => { closeMenu(true); run(); });
        menu.append(item);
      }
      menu.addEventListener('contextmenu', event => event.preventDefault());
      menu.addEventListener('keydown', event => {
        const entries = [...menu.children];
        const at = entries.indexOf(event.target);
        const move = {ArrowDown: at + 1, ArrowUp: at - 1, Home: 0, End: entries.length - 1}[event.key];
        if (move !== undefined) entries[(move + entries.length) % entries.length].focus();
        else if (event.key === 'Enter' || event.key === ' ') event.target.click();
        else if (event.key === 'Escape') closeMenu(true);
        else if (event.key === 'Tab') closeMenu(true);
        else return;
        event.preventDefault();
        event.stopPropagation();
      });
      const host = root.querySelector('[data-database-workspace]');
      const box = host.getBoundingClientRect();
      menu.style.left = `${Math.max(0, Math.min(x - box.left, box.width - 190))}px`;
      menu.style.top = `${Math.max(0, Math.min(y - box.top, box.height - items.length * 26 - 8))}px`;
      host.append(menu);
      window.uiKitIcons?.enhance?.(menu);
      menu.firstElementChild.focus();
    }
    function toggle(key) {
      filterExpanded.delete(key);
      if (expanded.has(key)) expanded.delete(key); else expanded.add(key);
      renderTables();
    }
    function setAll(open) {
      expanded.clear();
      filterExpanded.clear();
      if (open) for (const table of state?.tables || []) {
        const key = `t:${table.name}`;
        expanded.add(key);
        expanded.add(`${key}:columns`);
        expanded.add(`${key}:indexes`);
        expanded.add(`${key}:foreigns`);
      }
      renderTables();
    }
    let focusKey = null;
    const focusRow = node => node?.focus();
    function parentRow(node) {
      const level = Number(node.getAttribute('aria-level'));
      let other = node.previousElementSibling;
      while (other && Number(other.getAttribute('aria-level')) >= level) other = other.previousElementSibling;
      return other;
    }
    function rowKeys(event, node, row) {
      const rows = [...node.parentElement.children];
      const at = rows.indexOf(node);
      const isOpen = expanded.has(row.key);
      if (event.key === 'ArrowDown') focusRow(rows[at + 1]);
      else if (event.key === 'ArrowUp') focusRow(rows[at - 1]);
      else if (event.key === 'Home') focusRow(rows[0]);
      else if (event.key === 'End') focusRow(rows[rows.length - 1]);
      else if (event.key === 'ArrowRight') { if (row.children && !isOpen) toggle(row.key); else if (isOpen) focusRow(rows[at + 1]); }
      else if (event.key === 'ArrowLeft') { if (row.children && isOpen) toggle(row.key); else focusRow(parentRow(node)); }
      else if (event.key === 'Enter') { if (row.open) open(row.open); else if (row.children) toggle(row.key); }
      else if (event.key === ' ') { if (row.children) toggle(row.key); }
      else if (event.key === 'ContextMenu' || (event.key === 'F10' && event.shiftKey)) {
        const box = node.getBoundingClientRect();
        showMenu(box.left + 24, box.bottom, row, node);
      }
      else return;
      event.preventDefault();
    }
    const filterExpanded = new Set();
    function filterTables() {
      const query = $('dbw-table-filter').value.trim().toLowerCase();
      for (const key of filterExpanded) expanded.delete(key);
      filterExpanded.clear();
      if (query) for (const table of state?.tables || []) {
        if (table.name.toLowerCase().includes(query) || !table.columns.some(column => column.name.toLowerCase().includes(query))) continue;
        for (const key of [`t:${table.name}`, `t:${table.name}:columns`]) {
          if (expanded.has(key)) continue;
          expanded.add(key);
          filterExpanded.add(key);
        }
      }
      renderTables();
    }
    function renderUnreadable() {
      const files = state?.unreadable || [];
      const note = $('dbw-unreadable');
      note.hidden = !files.length;
      note.textContent = files.length ? format('unreadable', '{count} migration files could not be read and are left out: {files}', {count: files.length, files: files.map(path => path.split('/').pop()).join(', ')}) : '';
    }
    function renderTables() {
      const query = $('dbw-table-filter').value.trim().toLowerCase();
      const all = state?.tables || [];
      const shown = all.filter(table => !query
        || table.name.toLowerCase().includes(query)
        || table.models.some(model => model.toLowerCase().includes(query))
        || table.columns.some(column => column.name.toLowerCase().includes(query)));
      $('dbw-table-count').textContent = query ? `${shown.length} / ${all.length}` : `${all.length}`;
      if (!all.length) return $('dbw-tables').replaceChildren(empty(tr('noTables', 'No table is created by the migrations of this project yet.')));
      if (!shown.length) return $('dbw-tables').replaceChildren(empty(tr('noMatches', 'No table matches the filter.')));
      const hadFocus = $('dbw-tables').contains(root.activeElement ?? document.activeElement);
      const table = el('table', null, 'ui-table dbw-table dbw-tree');
      table.setAttribute('role', 'treegrid');
      table.setAttribute('aria-label', tr('tabTables', 'Tables'));
      const head = el('tr');
      head.append(el('th', tr('table', 'Table')), el('th', tr('details', 'Details')));
      const thead = el('thead');
      thead.append(head);
      const body = el('tbody');
      let roving = null;
      for (const source of shown) {
        for (const row of treeRows(source, query)) {
          const tr_ = el('tr', null, `dbw-tree-row dbw-depth-${row.depth}${row.hit ? ' dbw-hit' : ''}`);
          tr_.dataset.key = row.key;
          tr_.tabIndex = -1;
          tr_.setAttribute('aria-level', String(row.depth + 1));
          if (row.children) tr_.setAttribute('aria-expanded', String(expanded.has(row.key)));
          if (!roving || row.key === focusKey) roving = tr_;
          const first = el('td');
          first.setAttribute('role', 'gridcell');
          const line = el('div', null, 'dbw-tree-cell');
          line.style.paddingLeft = `${row.depth * 20}px`;
          const slot = el('span', null, 'dbw-tree-slot');
          if (row.children) {
            slot.append(icon(expanded.has(row.key) ? 'chevron-down' : 'chevron-right'));
            slot.addEventListener('click', event => { event.stopPropagation(); toggle(row.key); });
          }
          line.append(slot);
          if (row.icon) line.append(icon(row.icon));
          if (row.badge !== undefined) line.append(el('span', row.badge, `dbw-badge dbw-badge-${row.keyKind || 'none'}`));
          line.append(el('span', row.label, 'dbw-tree-label'));
          first.append(line);
          const second = el('td', row.details || '', 'dbw-muted-cell');
          second.setAttribute('role', 'gridcell');
          tr_.append(first, second);
          tr_.addEventListener('focus', () => {
            for (const other of body.children) other.tabIndex = other === tr_ ? 0 : -1;
            focusKey = row.key;
          });
          tr_.addEventListener('keydown', event => rowKeys(event, tr_, row));
          if (row.children) tr_.addEventListener('click', () => toggle(row.key));
          tr_.addEventListener('contextmenu', event => {
            event.preventDefault();
            showMenu(event.clientX, event.clientY, row, tr_);
          });
          if (row.open) {
            tr_.title = tr('openMigration', 'Open the migration');
            tr_.addEventListener('dblclick', () => open(row.open));
          }
          body.append(tr_);
        }
      }
      roving.tabIndex = 0;
      table.append(thead, body);
      $('dbw-tables').replaceChildren(table);
      window.uiKitIcons?.enhance?.($('dbw-tables'));
      if (hadFocus) roving.focus();
    }

    /* ---------- Compare ---------- */
    function renderCompare() {
      const live = state?.live;
      const target = $('dbw-compare-result');
      if (!live) {
        $('dbw-compare-summary').textContent = '';
        return target.replaceChildren(empty(tr('compareIdle', 'Run the comparison to see pending migrations and differences.')));
      }
      const filter = $('dbw-compare-status').value;
      const rows = live.rows.filter(row => !filter || row.status === filter);
      $('dbw-compare-summary').textContent = `${live.connection} (${live.driver}) · ${format('differences', '{count} differences', {count: live.rows.length})} · ${format('databaseTables', '{count} tables in the database', {count: live.tables.length})}${live.truncated ? ' · ' + tr('truncated', 'list truncated') : ''}`;
      if (!live.rows.length) return target.replaceChildren(empty(tr('inSync', 'The database matches the migrations.')));
      if (!rows.length) return target.replaceChildren(empty(tr('noCompareMatches', 'Nothing matches the filter.')));
      target.replaceChildren(grid([tr('table', 'Table'), tr('status', 'Status'), tr('details', 'Details')], rows.map(row => {
        const badge = el('span', STATUS[row.status] ? tr(...STATUS[row.status]) : row.status, `ui-badge dbw-status-${row.status}`);
        const details = el('ul', null, 'dbw-details');
        for (const detail of row.details) details.append(el('li', detail));
        return [row.open ? openLink(row.table, row.open) : (row.table || '—'), badge, details];
      })));
    }

    /* ---------- Rows ---------- */
    function pager() {
      const disabled = busy();
      const reply = rowsReply;
      $('dbw-rows-prev').disabled = disabled || !reply || reply.page <= 0;
      $('dbw-rows-next').disabled = disabled || !reply || (reply.page + 1) * reply.size >= reply.total;
    }
    function requestRows(page) {
      const table = $('dbw-rows-table').value.trim();
      if (!table) return setError(tr('chooseTable', 'Choose a table.'));
      rowsRequest = {op: 'rows', table, page, size: Number($('dbw-rows-size').value)};
      send(rowsRequest);
    }
    function renderRows() {
      const target = $('dbw-rows-result');
      if (!rowsReply) {
        $('dbw-rows-summary').textContent = '';
        return target.replaceChildren(empty(tr('rowsIdle', 'Choose a table to read its rows.')));
      }
      const reply = rowsReply;
      const from = reply.rows.length ? reply.page * reply.size + 1 : 0;
      $('dbw-rows-summary').textContent = `${from}–${reply.page * reply.size + reply.rows.length} / ${reply.total}${reply.masked.length ? ' · ' + format('masked', 'masked: {columns}', {columns: reply.masked.join(', ')}) : ''}`;
      if (!reply.rows.length) return target.replaceChildren(empty(tr('noRows', 'This table has no rows.')));
      const masked = new Set(reply.masked);
      const headings = reply.columns.map(name => {
        const th = el('span', name);
        if (masked.has(name)) th.title = tr('maskedColumn', 'Masked: looks like a secret or personal data');
        return th;
      });
      target.replaceChildren(grid(headings, reply.rows.map(cells => cells.map(cell => {
        if (cell === null) return el('em', 'NULL', 'dbw-muted');
        return el('span', String(cell), 'dbw-cell');
      }))));
      const table = target.querySelector('table');
      table.classList.add('dbw-rows');
      table.querySelectorAll('thead th').forEach((th, index) => th.classList.toggle('dbw-masked', masked.has(reply.columns[index])));
    }
    let tableNames = [];
    let suggestIndex = -1;
    function renderTableChoices() {
      const names = new Set([...(state?.tables || []).map(table => table.name), ...(state?.live?.tables || [])]);
      tableNames = [...names].sort();
    }
    function suggest(show = true) {
      const input = $('dbw-rows-table');
      const list = $('dbw-rows-tables');
      const query = input.value.trim().toLowerCase();
      const matches = tableNames.filter(name => !query || name.toLowerCase().includes(query)).slice(0, 50);
      suggestIndex = -1;
      list.replaceChildren(...matches.map(name => {
        const item = el('li', name, 'dbw-suggest-item');
        item.setAttribute('role', 'option');
        item.addEventListener('mousedown', event => { event.preventDefault(); input.value = name; suggest(false); });
        return item;
      }));
      const open = show && matches.length > 0;
      list.hidden = !open;
      input.setAttribute('aria-expanded', String(open));
    }
    function moveSuggest(step) {
      const items = [...$('dbw-rows-tables').children];
      if (!items.length) return;
      suggestIndex = (suggestIndex + step + items.length) % items.length;
      items.forEach((item, index) => item.classList.toggle('active', index === suggestIndex));
      items[suggestIndex].scrollIntoView({block: 'nearest'});
    }

    /* ---------- Schema diagram ---------- */
    function layout(diagram, width, height) {
      const parents = new Map(diagram.tables.map(table => [table.name, new Set()]));
      const linked = new Set();
      for (const link of diagram.links) { linked.add(link.from); linked.add(link.to); }
      for (const link of diagram.links) if (!link.dashed && link.from !== link.to && parents.has(link.from)) parents.get(link.from).add(link.to);
      const ranks = new Map();
      const rankOf = (name, seen) => {
        if (ranks.has(name)) return ranks.get(name);
        if (seen.has(name)) return 0;
        seen.add(name);
        let rank = 0;
        for (const parent of parents.get(name) || []) if (parents.has(parent)) rank = Math.max(rank, rankOf(parent, seen) + 1);
        ranks.set(name, rank);
        return rank;
      };
      const heightOf = table => BOX.head + Math.max(1, Math.min(table.columns.length, BOX.rows) + (table.columns.length > BOX.rows ? 1 : 0)) * BOX.row + 8;
      let top = 0;
      for (const table of diagram.tables) if (linked.has(table.name)) top = Math.max(top, rankOf(table.name, new Set()));
      for (const table of diagram.tables) if (!linked.has(table.name)) ranks.set(table.name, top + 1);
      const area = diagram.tables.reduce((sum, table) => sum + (heightOf(table) + BOX.gapY) * (BOX.width + BOX.gapX), 0);
      const limit = Math.max(Math.max(...diagram.tables.map(heightOf)), Math.sqrt(area * Math.max(0.2, height / Math.max(1, width))));
      const byRank = new Map();
      for (const table of diagram.tables) {
        const rank = ranks.get(table.name) || 0;
        if (!byRank.has(rank)) byRank.set(rank, []);
        byRank.get(rank).push(table);
      }
      const positions = new Map();
      let x = 0;
      for (const rank of [...byRank.keys()].sort((a, b) => a - b)) {
        let y = 0;
        for (const table of byRank.get(rank).sort((a, b) => a.name.localeCompare(b.name))) {
          const h = heightOf(table);
          if (y > 0 && y + h > limit) { x += BOX.width + BOX.gapX; y = 0; }
          positions.set(table.name, {x, y, w: BOX.width, h, table});
          y += h + BOX.gapY;
        }
        x += BOX.width + BOX.gapX * (rank === top ? 2 : 1);
      }
      return positions;
    }
    function drawDiagram() {
      const container = $('dbw-diagram');
      const diagram = state;
      if (!diagram?.tables?.length) {
        renderDiagram = null;
        $('dbw-count').textContent = '';
        return container.replaceChildren(empty(tr('noTables', 'No table is created by the migrations of this project yet.')));
      }
      const bounds = container.getBoundingClientRect();
      const positions = layout(diagram, bounds.width || 1200, bounds.height || 500);
      const canvas = svg('svg', {role: 'group', 'aria-label': tr('diagram', 'Schema diagram')});
      const world = svg('g');
      canvas.append(world);
      const links = [];
      const boxes = new Map();
      for (const link of diagram.links) {
        const a = positions.get(link.from);
        const b = positions.get(link.to);
        if (!a || !b) continue;
        const index = link.column ? a.table.columns.findIndex(column => column.name === link.column) : -1;
        const ay = a.y + (index >= 0 && index < BOX.rows ? BOX.head + index * BOX.row + BOX.row / 2 + 4 : BOX.head / 2);
        const by = b.y + BOX.head / 2;
        const right = b.x >= a.x;
        const same = b.x === a.x;
        const ax = right ? a.x + a.w : a.x;
        const bx = same ? b.x + b.w : right ? b.x : b.x + b.w;
        const bend = same ? 60 : Math.max(40, Math.abs(bx - ax) / 2);
        const c1 = right || same ? ax + bend : ax - bend;
        const c2 = same ? bx + bend : right ? bx - bend : bx + bend;
        const description = link.dashed ? link.kind : `${link.from}.${link.column} → ${link.to}.${link.references}`;
        const path = svg('path', {d: `M${ax},${ay} C${c1},${ay} ${c2},${by} ${bx},${by}`, class: `dbw-link-line${link.dashed ? ' dbw-relation' : ''}`}, svg('title', {}, description));
        path.dataset.from = link.from;
        path.dataset.to = link.to;
        links.push(path);
        world.append(path);
      }
      for (const [name, position] of positions) {
        const table = position.table;
        const box = svg('g', {class: 'dbw-box', transform: `translate(${position.x},${position.y})`});
        box.append(svg('rect', {class: 'dbw-frame', width: position.w, height: position.h, rx: 6}));
        const title = svg('g', {class: 'dbw-title', tabindex: table.open ? 0 : -1, role: table.open ? 'button' : 'img', 'aria-label': `${name}${table.models.length ? ' (' + table.models.join(', ') + ')' : ''}`},
          svg('title', {}, `${table.models.length ? table.models.join(', ') + ' · ' : ''}${table.open ? tr('openMigration', 'Open the migration') : ''}`),
          svg('rect', {width: position.w, height: BOX.head, rx: 6}),
          svg('text', {x: 10, y: 17}, name + (table.models.length ? `  ·  ${table.models.join(', ')}` : '')));
        const activate = event => { event.stopPropagation(); open(table.open); };
        title.addEventListener('click', activate);
        title.addEventListener('keydown', event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); activate(event); } });
        box.append(title);
        table.columns.slice(0, BOX.rows).forEach((column, row) => {
          const y = BOX.head + row * BOX.row + 14;
          box.append(svg('text', {class: column.key ? `dbw-col dbw-key-${column.key}` : 'dbw-col', x: 10, y}, `${column.key ? keyLabel(column.key) + ' ' : ''}${column.name}`));
          box.append(svg('text', {class: 'dbw-kind', x: position.w - 8, y, 'text-anchor': 'end'}, column.kind));
        });
        if (table.columns.length > BOX.rows) box.append(svg('text', {class: 'dbw-kind', x: 10, y: BOX.head + BOX.rows * BOX.row + 14}, format('more', '+{count} more', {count: table.columns.length - BOX.rows})));
        box.addEventListener('mouseenter', () => links.forEach(link => link.classList.toggle('on', link.dataset.from === name || link.dataset.to === name)));
        box.addEventListener('mouseleave', () => links.forEach(link => link.classList.remove('on')));
        boxes.set(name, box);
        world.append(box);
      }
      const view = diagramViews.get(projectKey) || {x: 20, y: 20, k: 1, fresh: true};
      diagramViews.set(projectKey, view);
      const apply = () => world.setAttribute('transform', `translate(${view.x},${view.y}) scale(${view.k})`);
      const zoomAt = (factor, cx, cy) => {
        const k = Math.min(3, Math.max(0.15, view.k * factor));
        view.x = cx - (cx - view.x) * (k / view.k);
        view.y = cy - (cy - view.y) * (k / view.k);
        view.k = k;
        apply();
      };
      canvas.addEventListener('wheel', event => {
        event.preventDefault();
        const rect = canvas.getBoundingClientRect();
        zoomAt(event.deltaY < 0 ? 1.1 : 1 / 1.1, event.clientX - rect.left, event.clientY - rect.top);
      }, {passive: false});
      let drag = null;
      canvas.addEventListener('pointerdown', event => { drag = {x: event.clientX, y: event.clientY, vx: view.x, vy: view.y}; container.classList.add('dbw-dragging'); canvas.setPointerCapture?.(event.pointerId); });
      canvas.addEventListener('pointermove', event => { if (!drag) return; view.x = drag.vx + event.clientX - drag.x; view.y = drag.vy + event.clientY - drag.y; apply(); });
      const stop = () => { drag = null; container.classList.remove('dbw-dragging'); };
      canvas.addEventListener('pointerup', stop);
      canvas.addEventListener('pointercancel', stop);
      const fit = () => {
        const rect = canvas.getBoundingClientRect();
        let w = 0;
        let h = 0;
        for (const position of positions.values()) { w = Math.max(w, position.x + position.w); h = Math.max(h, position.y + position.h); }
        view.k = Math.min(1.2, Math.max(0.15, Math.min((rect.width - 40) / w, (rect.height - 40) / h)));
        view.x = 20;
        view.y = 20;
        apply();
      };
      const filter = () => {
        const query = $('dbw-diagram-filter').value.trim().toLowerCase();
        const hits = new Set();
        if (query) for (const [name, position] of positions) {
          if (name.toLowerCase().includes(query) || position.table.models.some(model => model.toLowerCase().includes(query)) || position.table.columns.some(column => column.name.toLowerCase().includes(query))) hits.add(name);
        }
        for (const [name, box] of boxes) { box.classList.toggle('dim', !!query && !hits.has(name)); box.classList.toggle('hit', !!query && hits.has(name)); }
        for (const link of links) link.classList.toggle('dim', !!query && !hits.has(link.dataset.from) && !hits.has(link.dataset.to));
      };
      container.replaceChildren(canvas);
      renderDiagram = {fit, filter, zoom: factor => { const rect = canvas.getBoundingClientRect(); zoomAt(factor, rect.width / 2, rect.height / 2); }};
      filter();
      $('dbw-count').textContent = `${format('tables', '{count} tables', {count: diagram.tables.length})} · ${format('links', '{count} links', {count: diagram.links.length})}`;
      if (view.fresh) { requestAnimationFrame(fit); view.fresh = false; } else apply();
    }

    /* ---------- wiring ---------- */
    function render() {
      if (!root.querySelector('[data-dbw-panel="schema"]').hidden) drawDiagram();
      renderUnreadable();
      renderTables();
      renderCompare();
      renderTableChoices();
      renderRows();
    }
    const dismissMenu = event => { if (menu && !menu.contains(event.target)) closeMenu(); };
    const escapeMenu = event => { if (event.key === 'Escape' && menu) closeMenu(true); };
    document.addEventListener('pointerdown', dismissMenu);
    document.addEventListener('keydown', escapeMenu);
    if (hasDiagram) {
      $('dbw-diagram-filter').addEventListener('input', () => renderDiagram?.filter());
      $('dbw-fit').addEventListener('click', () => renderDiagram?.fit());
      $('dbw-zoom-in').addEventListener('click', () => renderDiagram?.zoom(1.25));
      $('dbw-zoom-out').addEventListener('click', () => renderDiagram?.zoom(1 / 1.25));
      $('dbw-refresh').addEventListener('click', () => send({op: 'state'}));
    }
    for (const tab of root.querySelectorAll('[data-dbw-tab]')) {
      tab.addEventListener('click', () => {
        for (const other of root.querySelectorAll('[data-dbw-tab]')) other.setAttribute('aria-selected', String(other === tab));
        for (const panel of root.querySelectorAll('[data-dbw-panel]')) panel.hidden = panel.dataset.dbwPanel !== tab.dataset.dbwTab;
        if (tab.dataset.dbwTab === 'schema') drawDiagram();
      });
    }
    $('dbw-table-filter').addEventListener('input', filterTables);
    $('dbw-table-refresh').addEventListener('click', () => send({op: 'state'}));
    $('dbw-expand-all').addEventListener('click', () => setAll(true));
    $('dbw-collapse-all').addEventListener('click', () => setAll(false));
    $('dbw-compare').addEventListener('click', () => send({op: 'compare'}));
    $('dbw-compare-status').addEventListener('change', renderCompare);
    $('dbw-rows-load').addEventListener('click', () => requestRows(0));
    const tableInput = $('dbw-rows-table');
    tableInput.addEventListener('input', () => suggest());
    tableInput.addEventListener('focus', () => suggest());
    tableInput.addEventListener('blur', () => suggest(false));
    tableInput.addEventListener('keydown', event => {
      const list = $('dbw-rows-tables');
      if (event.key === 'ArrowDown') { event.preventDefault(); if (list.hidden) suggest(); moveSuggest(1); }
      else if (event.key === 'ArrowUp') { event.preventDefault(); moveSuggest(-1); }
      else if (event.key === 'Escape') suggest(false);
      else if (event.key === 'Enter') {
        if (!list.hidden && suggestIndex >= 0) tableInput.value = list.children[suggestIndex].textContent;
        suggest(false);
        requestRows(0);
      }
    });
    $('dbw-rows-prev').addEventListener('click', () => rowsReply && requestRows(rowsReply.page - 1));
    $('dbw-rows-next').addEventListener('click', () => rowsReply && requestRows(rowsReply.page + 1));
    bridge.onMessage(raw => {
      if (disposed) return;
      let reply;
      try { reply = JSON.parse(raw); } catch { return; }
      queue.shift();
      if (reply.error) { setError(reply.error); return refreshControls(); }
      if (reply.rows) rowsReply = reply.rows;
      else if (reply.tables) state = reply;
      else if (reply.live) state = {...state, live: reply.live};
      refreshControls();
      render();
    });
    render();
    send({op: 'state'});
    return () => {
      disposed = true;
      closeMenu();
      document.removeEventListener('pointerdown', dismissMenu);
      document.removeEventListener('keydown', escapeMenu);
    };
  }};
})();
