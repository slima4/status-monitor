import {COLUMNS, NODES, FLOWS} from "./_data.js";
import {wireGeometry, BADGE_R} from "./_layout.js";

/* ── render ───────────────────────────────────────── */
const root = document.querySelector('.archmap');
const colsEl = document.getElementById('cols');
const nodeEl = {};
for (const c of COLUMNS){
  const d = document.createElement('div');
  d.className = 'col';
  d.innerHTML = `<h2>${c.label}</h2>`;
  for (const n of NODES.filter(n=>n.col===c.id)){
    const b = document.createElement('div');
    b.className='node'; b.style.setProperty('--nc', c.color); b.dataset.id=n.id;
    b.tabIndex = 0; b.setAttribute('role','button');
    b.innerHTML = `<b>${n.t}</b><span>${n.s}</span>`;
    b.addEventListener('click', ()=>filterByNode(n.id));
    b.addEventListener('keydown', e=>{ if(e.key==='Enter'||e.key===' '){e.preventDefault();filterByNode(n.id);} });
    d.appendChild(b); nodeEl[n.id]=b;
  }
  colsEl.appendChild(d);
}

const flowListEl = document.getElementById('flowList');
const flowEl = {};
for (const f of FLOWS){
  const d = document.createElement('div');
  d.className='flow'; d.dataset.id=f.id;
  d.tabIndex = 0; d.setAttribute('role','button');
  d.innerHTML = `<b>${f.name}</b><span>${f.desc}</span>`;
  d.addEventListener('click', ()=>select(f.id));
  d.addEventListener('keydown', e=>{ if(e.key==='Enter'||e.key===' '){e.preventDefault();select(f.id);} });
  d.addEventListener('focus', ()=>d.scrollIntoView({block:'nearest'}));
  flowListEl.appendChild(d); flowEl[f.id]=d;
}

const svg = document.getElementById('wires');
const badges = document.getElementById('badges');
const stepListEl = document.getElementById('stepList');
const title = id => (NODES.find(n=>n.id===id)||{t:id}).t;
let current = null, nodeFilter = null;

function select(id){
  current = FLOWS.find(f=>f.id===id) || null;
  reset(false);
  root.classList.toggle('sel', !!current);
  for (const f of FLOWS) flowEl[f.id].classList.toggle('on', !!current && f.id===current.id);
  const live = new Set();
  if (current) current.steps.forEach(s=>{live.add(s.f); live.add(s.t);});
  for (const n of NODES) nodeEl[n.id].classList.toggle('on', live.has(n.id));
  renderSteps();
  draw();
}

function renderSteps(){
  stepListEl.innerHTML = '';
  if (!current){
    stepListEl.innerHTML = '<div class="empty">No flow selected. Pick one above — the map will show the ordered hops, and hovering a step thickens its wire.</div>';
    return;
  }
  current.steps.forEach((s,i)=>{
    const d = document.createElement('div');
    d.className='step';
    d.innerHTML = `<div class="n">${i+1}</div><div>
      <div class="hop"><b>${title(s.f)}</b> → <b>${title(s.t)}</b></div>
      <div class="t">${s.h}</div><div class="d">${s.d}</div></div>`;
    d.tabIndex = 0; d.setAttribute('role','button');
    d.addEventListener('mouseenter', ()=>hot(i,true));
    d.addEventListener('mouseleave', ()=>hot(i,false));
    d.addEventListener('click', ()=>jump(i));
    d.addEventListener('keydown', e=>{ if(e.key==='Enter'||e.key===' '){e.preventDefault();jump(i);} });
    stepListEl.appendChild(d);
  });
}

function hot(i, on){
  // Playback owns the wire styling, but clearing must always get through:
  // hover can be applied before Play starts and would otherwise stick.
  if (on && player.active) return;
  svg.querySelectorAll('path').forEach((p,j)=>{
    p.classList.toggle('hot', on && j===i);
    p.classList.toggle('dim', on && j!==i);
  });
  if (!current) return;
  const s = current.steps[i];
  nodeEl[s.f].classList.toggle('focus', on);
  nodeEl[s.t].classList.toggle('focus', on);
}

function filterByNode(id){
  const hits = FLOWS.filter(f=>f.steps.some(s=>s.f===id||s.t===id)).map(f=>f.id);
  if (!hits.length) return;
  if (nodeFilter === id){                          // re-click same node: cycle the flows it touches
    if (hits.length === 1) return;                 // only one flow, nothing to cycle
    const idx = current ? hits.indexOf(current.id) : -1;
    select(hits[(idx + 1) % hits.length]);
    return;
  }
  nodeFilter = id;                                 // new node: filter the list, select the first
  for (const f of FLOWS) flowEl[f.id].classList.toggle('hide', !hits.includes(f.id));
  select(hits[0]);
}

document.getElementById('clear').addEventListener('click', ()=>{
  nodeFilter = null;
  for (const f of FLOWS) flowEl[f.id].classList.remove('hide');
  select(null);
});

/* ── wires ────────────────────────────────────────── */
const NS='http://www.w3.org/2000/svg';
let pathEls = [], badgeEls = [];
function draw(){
  svg.innerHTML=''; badges.innerHTML='';
  pathEls = []; badgeEls = [];
  if (!current){ paintState(); return; }
  const host = svg.getBoundingClientRect();
  const box = el => { const r = el.getBoundingClientRect();
    return {l:r.left-host.left, r:r.right-host.left, t:r.top-host.top, b:r.bottom-host.top,
            cx:(r.left+r.right)/2-host.left, cy:(r.top+r.bottom)/2-host.top}; };
  const boxes = {};
  for (const n of NODES) boxes[n.id] = box(nodeEl[n.id]);

  wireGeometry(current.steps, boxes, Object.values(boxes)).forEach((w,i)=>{
    const p=document.createElementNS(NS,'path'); p.setAttribute('d',w.d); svg.appendChild(p);
    pathEls.push(p);
    const g=document.createElementNS(NS,'g'); g.setAttribute('class','badge');
    const c=document.createElementNS(NS,'circle');
    c.setAttribute('cx',w.badge.x); c.setAttribute('cy',w.badge.y); c.setAttribute('r',BADGE_R); g.appendChild(c);
    const tx=document.createElementNS(NS,'text');
    tx.setAttribute('x',w.badge.x); tx.setAttribute('y',w.badge.y+0.5); tx.textContent=i+1; g.appendChild(tx);
    badges.appendChild(g); badgeEls.push(g);
  });
  paintState();
}

/* ── playback ─────────────────────────────────────── */
/* A signal walks the flow: one hop travels, the wire draws itself behind it,
   the node it lands on lights up, then a dwell long enough to read the step. */
const MS_PER_PX = 1.5, TRAVEL_MIN = 420, TRAVEL_MAX = 1400, DWELL_MS = 950;
const calm = matchMedia('(prefers-reduced-motion:reduce)');
const player = {active:false, playing:false, i:-1, phase:'idle', p:0, t0:0, dur:0, held:0, raf:0};

const pkt = document.createElementNS(NS,'circle');
pkt.setAttribute('class','pkt'); pkt.setAttribute('r','5');
const halo = document.createElementNS(NS,'circle');
halo.setAttribute('class','pkt-halo'); halo.setAttribute('r','12');

const playBtn = document.getElementById('play');
const prevBtn = document.getElementById('prev');
const nextBtn = document.getElementById('next');
const countEl = document.getElementById('stepCount');

function reset(active){
  cancelAnimationFrame(player.raf);
  Object.assign(player, {active:!!active, playing:false, i:-1, phase:'idle', p:0, t0:0, dur:0, held:0, raf:0});
}

function paintState(){
  root.classList.toggle('play', player.active);
  const done = player.phase === 'done';
  const upto = done ? player.i + 1 : player.i;
  pathEls.forEach((p,j)=>{
    if (p.len === undefined) p.len = p.getTotalLength();
    const now = player.active && !done && j === player.i;
    p.classList.toggle('done', player.active && j < upto);
    p.classList.toggle('now', now);
    if (!player.active || j < upto){ p.style.strokeDasharray=''; p.style.strokeDashoffset=''; }
    else { p.style.strokeDasharray = p.len; p.style.strokeDashoffset = now ? p.len*(1-player.p) : p.len; }
  });
  // A badge rides the middle of its wire, so it may only show once that wire
  // has been travelled — otherwise the number floats over nothing.
  const landed = player.phase === 'travel' ? player.i : player.i + 1;
  badgeEls.forEach((g,j)=>g.classList.toggle('seen', player.active && j < landed));
  paintNodes();
  paintSteps();
  paintTransport();
  movePkt();
}

function paintNodes(){
  const seen = new Set(), live = new Set();
  if (player.active && current){
    const arrived = player.phase !== 'travel';
    for (let j=0;j<=player.i;j++){
      const s = current.steps[j];
      seen.add(s.f);
      if (j < player.i || arrived) seen.add(s.t);
    }
    const s = current.steps[player.i];
    if (s && player.phase !== 'done'){ live.add(s.f); if (arrived) live.add(s.t); }
  }
  for (const n of NODES){
    nodeEl[n.id].classList.toggle('visited', seen.has(n.id));
    nodeEl[n.id].classList.toggle('live', live.has(n.id));
  }
}

function paintSteps(){
  const rows = stepListEl.children;
  for (let j=0;j<rows.length;j++){
    rows[j].classList.toggle('done', player.active && j < player.i);
    rows[j].classList.toggle('now', player.active && j === player.i);
  }
}

const PLAY_LABEL = {play:'Play the flow', pause:'Pause the flow', replay:'Replay the flow'};

function paintTransport(){
  const n = current ? current.steps.length : 0;
  const state = player.playing ? 'pause' : (player.phase === 'done' ? 'replay' : 'play');
  playBtn.dataset.state = state;
  playBtn.setAttribute('aria-label', PLAY_LABEL[state]);
  playBtn.title = PLAY_LABEL[state];
  countEl.textContent = !n ? '' : player.active ? `step ${Math.min(player.i+1,n)} of ${n}` : `${n} steps`;
  prevBtn.disabled = !player.active || player.i <= 0;
  nextBtn.disabled = !n || (player.active && player.i >= n-1);
}

function movePkt(){
  const p = pathEls[player.i];
  if (!(player.active && p && player.phase === 'travel' && !calm.matches)){
    halo.remove(); pkt.remove(); return;
  }
  const at = p.getPointAtLength(p.len * player.p);
  for (const c of [halo, pkt]){ c.setAttribute('cx', at.x); c.setAttribute('cy', at.y); }
  if (!pkt.isConnected) badges.append(halo, pkt);
}

// Restarting the ring cancels the running animation, so animationend never
// fires for it. A shared handler keeps that from stacking listeners on a node
// a flow lands on more than once.
const dropHit = e => e.currentTarget.classList.remove('hit');

function pulse(el){
  if (calm.matches) return;
  el.classList.remove('hit');
  void el.offsetWidth;                             // restart the ring on a repeat hop
  el.classList.add('hit');
  el.addEventListener('animationend', dropHit);
}

function enter(i){
  const p = pathEls[i];
  const len = p ? (p.len ??= p.getTotalLength()) : 300;
  Object.assign(player, {i, p:0, phase:'travel', held:0, t0:performance.now(),
    dur: calm.matches ? 0 : Math.min(TRAVEL_MAX, Math.max(TRAVEL_MIN, len*MS_PER_PX))});
  scrollToStep(i);
  if (player.dur === 0) arrive(); else paintState();
}

function arrive(){
  player.p = 1; player.phase = 'dwell'; player.t0 = performance.now(); player.held = 0;
  pulse(nodeEl[current.steps[player.i].t]);
  paintState();
}

function frame(){
  const p = pathEls[player.i];
  if (p) p.style.strokeDashoffset = p.len * (1 - player.p);
  movePkt();
}

function tick(now){
  player.raf = 0;
  if (!player.playing) return;
  if (player.phase === 'travel'){
    player.p = player.dur ? Math.min(1, (now - player.t0)/player.dur) : 1;
    if (player.p >= 1) arrive(); else frame();
  } else if (now - player.t0 >= DWELL_MS){
    if (player.i < current.steps.length - 1) enter(player.i + 1);
    else return finish();
  }
  player.raf = requestAnimationFrame(tick);
}

function finish(){
  player.playing = false; player.phase = 'done';
  cancelAnimationFrame(player.raf); player.raf = 0;
  paintState();
}

function play(){
  if (!current){
    const first = FLOWS.find(f=>!flowEl[f.id].classList.contains('hide'));
    if (!first) return;
    select(first.id);
  }
  if (!player.active || player.phase === 'done') reset(true);
  player.active = true; player.playing = true;
  if (player.phase === 'idle') enter(0);
  else { player.t0 = performance.now() - player.held; player.held = 0; paintState(); }
  if (!player.raf) player.raf = requestAnimationFrame(tick);
}

function pause(){
  if (!player.playing) return;
  player.playing = false;
  player.held = performance.now() - player.t0;
  cancelAnimationFrame(player.raf); player.raf = 0;
  paintState();
}

function toggle(){ player.playing ? pause() : play(); }

// Manual stepping lands on the hop fully travelled, so the map matches the text.
function jump(i){
  if (!current) return;
  const to = Math.min(current.steps.length - 1, Math.max(0, i));
  const forward = to > player.i;
  pause();
  Object.assign(player, {active:true, i:to, p:1, phase:'dwell', held:0, t0:performance.now()});
  scrollToStep(to);
  if (forward) pulse(nodeEl[current.steps[to].t]);
  paintState();
}

function scrollToStep(i){
  const row = stepListEl.children[i];
  if (row) row.scrollIntoView({block:'nearest', behavior: calm.matches ? 'auto' : 'smooth'});
}

playBtn.addEventListener('click', toggle);
prevBtn.addEventListener('click', ()=>jump(player.i - 1));
nextBtn.addEventListener('click', ()=>jump(player.i < 0 ? 0 : player.i + 1));

root.addEventListener('keydown', e=>{
  if (e.target.closest('.node,.flow,input,select')) return;
  if (e.key === 'ArrowRight'){ e.preventDefault(); jump(player.i < 0 ? 0 : player.i + 1); }
  else if (e.key === 'ArrowLeft'){ e.preventDefault(); jump(player.i - 1); }
  else if (e.key === ' ' && !e.target.closest('button,.step')){ e.preventDefault(); toggle(); }
});

new ResizeObserver(draw).observe(document.getElementById('map'));
window.addEventListener('resize', draw);
select(null);
