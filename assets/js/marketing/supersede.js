import { autoplay } from './_figure.js';

/* Cancelling a scheduled check without touching the heap.

   A heap ordered by due time is a line, so it is drawn as one: every pending
   entry sits at the second it is due, and the playhead is the clock. An edit
   pushes a new entry and leaves the old one exactly where it was, which is the
   whole point being made visible — the dead entry stays on the track, ahead of
   the head, costing a few bytes until its own time arrives and it falls out. */

const SWEEP_MS = 9000;
const SPAN = 70;                    // seconds of track drawn
const END = 50;                     // where the playhead stops
/* Wide enough that two blocks a whole block apart cannot touch on the narrowest
   card, where the rail is ~313px and a block ~31px. */
const MIN_GAP = 8;
const TICKS = [0, 10, 20, 30, 40, 50, 60];

/* Deliberately co-prime enough that no two entries fall due on the same second,
   which would put two blocks at one position on the track. */
const INTERVALS = {A: 20, B: 30, C: 45};

/* Scripted config changes. Pops are not scripted: they happen when the clock
   reaches an entry, which is the behaviour under test. */
const SCRIPT = [
  {t: 0, act: 'boot', note: 'three monitors scheduled, each on its own interval'},
  {t: 8, act: 'edit', id: 'B', at: 35, note: 'B is edited. A new entry is pushed and the old one is left where it lies'},
  {t: 32, act: 'delete', id: 'C', note: 'C is deleted. Its row leaves the map and its entry stays on the track'},
];

const OUTCOME = {
  fire: 'seq still current, dispatched',
  stale: 'seq superseded, dropped without a scan',
  gone: 'not in the map at all, dropped the same way',
};

/** One snapshot per second: the heap, the map, and everything settled so far. */
function simulate(){
  const frames = [];
  let heap = [];
  const live = new Map();
  const settled = [];
  let nextSeq = 0;
  let si = 0;
  let line = '';

  const push = (id, at) => {
    const seq = nextSeq++;
    live.set(id, seq);
    heap.push({id, seq, at});
  };

  for (let t = 0; t <= END; t++){
    while (si < SCRIPT.length && SCRIPT[si].t === t){
      const s = SCRIPT[si++];
      line = s.note;
      if (s.act === 'boot') for (const id of Object.keys(INTERVALS)) push(id, INTERVALS[id]);
      else if (s.act === 'edit') push(s.id, s.at);
      else live.delete(s.id);
    }

    heap.sort((a, b) => a.at - b.at || a.seq - b.seq);
    while (heap.length && heap[0].at <= t){
      const e = heap.shift();
      const cur = live.get(e.id);
      const kind = cur === e.seq ? 'fire' : cur === undefined ? 'gone' : 'stale';
      settled.push({...e, kind});
      line = `t+${e.at}s · ${e.id} seq ${e.seq} · ${OUTCOME[kind]}`;
      if (kind === 'fire') heap.push({id: e.id, seq: e.seq, at: e.at + INTERVALS[e.id]});
    }

    // Carried on the frame, so a dropped repaint cannot make the caption skip
    // an event the way a running accumulator would.
    frames.push({
      t, line,
      heap: heap.map(e => ({...e, doomed: live.get(e.id) !== e.seq})),
      live: new Map(live),
      settled: settled.slice(),
    });
  }
  return frames;
}

const FRAMES = simulate();

/* Two entries can fall due close together, so a second row keeps them apart.
   Both rows are checked: tracking one and assuming the other is free would
   stack a third entry straight onto the second. */
function rows(items){
  const sorted = items.slice().sort((a, b) => a.at - b.at || a.seq - b.seq);
  const lastAt = [-Infinity, -Infinity];
  return sorted.map(e => {
    const row = e.at - lastAt[0] >= MIN_GAP ? 0 : e.at - lastAt[1] >= MIN_GAP ? 1 : 0;
    lastAt[row] = e.at;
    return {...e, row};
  });
}

const pct = at => `${(at / SPAN) * 100}%`;

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');

  root.classList.add('mk-fig', 'mk-sup');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">one heap, one map</span>
      <span class="mk-fig__note">cancelling an entry you cannot delete</span>
    </div>

    <div class="mk-sup__readout">
      <p class="mk-sup__big"><span data-stat="clock">t+0s</span></p>
      <p class="mk-sup__live" data-role="live"></p>
    </div>

    <div class="mk-sup__track">
      <!-- The playhead and the blocks must share one containing block, or the
           head crosses an entry at a different moment than it touches it. -->
      <div class="mk-sup__grid">
        <div class="mk-sup__ticks" aria-hidden="true">
          ${TICKS.map(s => `<span data-at="${s}" style="left:${pct(s)}">t+${s}</span>`).join('')}
        </div>

        <p class="mk-sup__rail-label">heap · due next</p>
        <div class="mk-sup__rail" data-role="heap" role="img" aria-label="Pending heap entries by the second they are due"></div>

        <p class="mk-sup__rail-label">what the pop decided</p>
        <div class="mk-sup__rail mk-sup__rail--out" data-role="out" role="img" aria-label="Entries already popped, and what happened to each"></div>

        <span class="mk-sup__head" data-role="head" aria-hidden="true"></span>
      </div>
    </div>

    <p class="mk-fig__legend" aria-hidden="true">
      <span class="mk-fig__key mk-fig__key--wait"></span>current generation
      <span class="mk-fig__key mk-fig__key--idle"></span>superseded, still queued
      <span class="mk-fig__key mk-fig__key--ok"></span>dispatched
      <span class="mk-fig__key mk-fig__key--setup"></span>dropped
    </p>

    <dl class="mk-sup__stats">
      <div><dt>on the heap</dt><dd data-stat="heap">0</dd></div>
      <div><dt>live monitors</dt><dd data-stat="livec">0</dd></div>
      <div><dt>dispatched</dt><dd data-stat="fired">0</dd></div>
      <div><dt>dropped on pop</dt><dd data-stat="dropped">0</dd></div>
    </dl>

    <p class="mk-fig__verdict mk-sup__verdict" role="status" aria-live="polite"></p>

    <div class="mk-fig__foot mk-fig__foot--split">
      <button type="button" class="mk-fig__go" data-act="replay">replay the clock</button>
      <span class="mk-fig__prov">three monitors · generations from one global counter · modelled, not measured</span>
    </div>`;

  const stat = n => root.querySelector(`[data-stat="${n}"]`);
  const heapRail = root.querySelector('[data-role="heap"]');
  const outRail = root.querySelector('[data-role="out"]');
  const liveEl = root.querySelector('[data-role="live"]');
  const head = root.querySelector('[data-role="head"]');
  const verdict = root.querySelector('.mk-sup__verdict');
  const btn = root.querySelector('[data-act="replay"]');

  let raf = 0;
  let drawn = -1;

  function paint(t){
    // 51 snapshots across a 9s sweep is ~10 rAF frames each, and a repaint
    // rebuilds both rails. Only draw when the snapshot actually changes.
    const i = Math.max(0, Math.min(END, Math.round(t)));
    if (i === drawn) return;
    drawn = i;
    const f = FRAMES[i];

    heapRail.innerHTML = rows(f.heap).map(e => `
      <span class="mk-sup__blk" data-state="${e.doomed ? 'doomed' : 'live'}" data-row="${e.row}"
            style="left:${pct(e.at)}"
            title="${e.id} · seq ${e.seq} · due t+${e.at}s · ${e.doomed ? 'superseded, waiting to pop' : 'current generation'}">${e.id}/${e.seq}</span>`).join('');

    outRail.innerHTML = rows(f.settled).map(e => `
      <span class="mk-sup__blk" data-state="${e.kind === 'fire' ? 'fire' : 'drop'}" data-row="${e.row}"
            style="left:${pct(e.at)}"
            title="${e.id} · seq ${e.seq} · popped at t+${e.at}s · ${OUTCOME[e.kind]}">${e.id}/${e.seq}</span>`).join('');

    head.style.left = pct(f.t);
    liveEl.innerHTML = Object.keys(INTERVALS).map(id => {
      const seq = f.live.get(id);
      return `<span class="mk-sup__chip" data-state="${seq === undefined ? 'gone' : 'held'}">${id} ${seq === undefined ? 'removed' : `seq ${seq}`}</span>`;
    }).join('');

    stat('clock').textContent = `t+${f.t}s`;
    stat('heap').textContent = f.heap.length;
    stat('livec').textContent = f.live.size;
    stat('fired').textContent = f.settled.filter(e => e.kind === 'fire').length;
    stat('dropped').textContent = f.settled.filter(e => e.kind !== 'fire').length;

    verdict.textContent = f.t >= END
      ? 'Both stale entries fell out on their own. The heap is back to one entry per live monitor, and nothing ever scanned it to get there.'
      : f.line;
  }

  function play(){
    cancelAnimationFrame(raf);
    drawn = -1;
    if (calm.matches){ paint(END); return; }
    const start = performance.now();
    const step = now => {
      const p = Math.min(1, (now - start) / SWEEP_MS);
      paint(p * END);
      if (p < 1) raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
  }

  btn.addEventListener('click', play);
  paint(END);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-supersede').forEach(mount);
