import { mover, paintTransport, picker, timers, transport } from './_figure.js';

/* Region vote, replayed round by round.

   Regions check independently, so the figure runs them that way: every round
   each reporting region walks its own chip through send → wait → result at the
   same time, and only a region that fails twice in a row sends a second chip
   to the ballot. Both gates are counted with the rules the incident writer
   uses, so the figure cannot drift from the product. */

const CONFIRMATIONS = 2;                           // fails in a row before a region votes
const START_MIN = 10 * 60 + 21;                    // clock the replay starts from
const ROUND_MS = 1900;
const STAGGER_MS = 70;                             // probes are independent, so they land apart
// What one check walks through, named the way the monitor page names it, and
// how long the chip takes over each hop.
const STAGES = [
  {key: 'dns',  label: 'dns'},
  {key: 'tcp',  label: 'tcp'},
  {key: 'tls',  label: 'tls'},
  {key: 'ttfb', label: 'ttfb'},
  {key: 'res',  label: 'result'},
];
const LEG = [130, 130, 140, 200, 160];
const LEG_VOTE = 420;
const LOG_LINES = 4;

const REGIONS = [
  {name: 'eu-helsinki', ticks: 'ooxxxx', ms: [141, 138,  92,  88,  90,  86]},
  {name: 'us-east',     ticks: 'oooxxx', ms: [126, 131, 129,  97,  94,  91]},
  {name: 'apac-sg',     ticks: 'ooooxx', ms: [210, 204, 219, 207, 120, 118]},
  {name: 'sa-east',     ticks: 'ooxooo', ms: [188, 192,  74, 190, 186, 184]},
  {name: 'af-south',    ticks: '------', ms: []},
];

const ROUNDS = REGIONS[0].ticks.length;
const clock = round => `${String(Math.floor((START_MIN + round) / 60)).padStart(2, '0')}:${String((START_MIN + round) % 60).padStart(2, '0')}`;

// Where every region stands after `round`, counted the way the incident writer
// counts it: a run of failures per region, and how many regions answered.
function tally(round){
  const rows = REGIONS.map(r => {
    const seen = [...r.ticks.slice(0, round + 1)].filter(c => c !== '-');
    let run = 0;
    for (let i = seen.length - 1; i >= 0 && seen[i] === 'x'; i--) run++;
    return {name: r.name, reporting: seen.length > 0, run, down: run >= CONFIRMATIONS};
  });
  return {rows, reporting: rows.filter(r => r.reporting).length};
}

const FINAL = tally(ROUNDS - 1);
const SEATS = FINAL.reporting;

// The detection rules a monitor can carry, named the way the monitor form
// names them and counted the way the policy counts them. A fixed count for the
// last seat is left out: it would only restate "all regions".
const POLICIES = [
  {value: 'any',      label: 'any region down',     need: () => 1},
  {value: 'majority', label: 'majority of regions', need: n => Math.floor(n / 2) + 1},
  {value: 'all',      label: 'all regions',         need: n => n},
  ...Array.from({length: SEATS - 1}, (_, i) => ({
    value: String(i + 1),
    label: `${i + 1} region${i ? 's' : ''}`,
    need: () => i + 1,
  })),
];
const DEFAULT_POLICY = 'majority';

// Seats fill in the order regions confirm, so a scrubbed frame and a played
// one agree on which name sits where.
const CONFIRM_AT = REGIONS.map(r => {
  for (let i = 0; i < ROUNDS; i++) if (tally(i).rows.find(x => x.name === r.name).down) return i;
  return Infinity;
});
const ORDER = REGIONS.map((r, i) => i).filter(i => CONFIRM_AT[i] < Infinity)
  .sort((a, b) => CONFIRM_AT[a] - CONFIRM_AT[b]);
const REPORTING = REGIONS.map((r, i) => i).filter(i => REGIONS[i].ms.length);
const SILENT = REGIONS.filter(r => !r.ms.length);

const state = row => !row.reporting ? 'no data'
  : row.down ? 'votes down'
  : row.run ? `fail ${row.run} of ${CONFIRMATIONS}`
  : 'passing';

// One line per check, the way a probe would report it.
function logLine(ri, round){
  const r = REGIONS[ri], pass = r.ticks[round] === 'o';
  const t = tally(round).rows[ri];
  const note = t.down ? 'confirmed down · votes'
    : t.run ? `fail ${t.run} of ${CONFIRMATIONS} · holding`
    : 'ok';
  return {time: clock(round), name: r.name, code: pass ? '200' : '503', ms: `${r.ms[round]}ms`, note, pass};
}

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  let policy = POLICIES.find(p => p.value === DEFAULT_POLICY);
  const needNow = () => policy.need(SEATS);

  root.classList.add('mk-fig', 'mk-quorum');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">vote replay</span>
      <span class="mk-fig__note">confirm ${CONFIRMATIONS} in a row</span>
      <span class="mk-fig__meta"><b class="mk-fig__clock">${clock(0)}</b> · <span class="mk-quorum__checks">0 checks</span></span>
    </div>

    <div class="mk-fig__lead">
      <p class="mk-fig__score">
        <span class="mk-fig__big">0</span><span class="mk-fig__need">/${needNow()}</span>
        <span class="mk-fig__unit">regions down of the ${SEATS} reporting</span>
      </p>
      ${transport('Vote playback', `${ROUNDS} rounds`)}
    </div>

    <div class="mk-quorum__flow">
      <div class="mk-quorum__probes">
        <p class="mk-fig__colhead">GET api.example.com/health · every region, every minute</p>
        <p class="mk-quorum__phases" aria-hidden="true">
          <span></span>
          <span class="mk-quorum__phaserow">${STAGES.map((s, si) =>
            `${si ? '<i class="mk-quorum__arrow"></i>' : ''}<span class="mk-quorum__phase">${s.label}</span>`).join('')}</span>
          <span class="mk-fig__pips">${Array.from({length: CONFIRMATIONS}, () =>
            '<i class="mk-fig__pip"></i>').join('')}</span>
          <span></span>
        </p>
        <div class="mk-quorum__lanes">${REGIONS.map((r, ri) => `
          <div class="mk-quorum__lane" data-lane="${ri}">
            <span class="mk-quorum__name">${r.name}</span>
            <span class="mk-quorum__pipe" aria-hidden="true">${STAGES.map((s, si) =>
              `${si ? '<i class="mk-quorum__arrow"></i>' : ''}<i class="mk-quorum__cell" data-stage="${s.key}"></i>`).join('')}</span>
            <span class="mk-fig__pips" aria-hidden="true">${Array.from({length: CONFIRMATIONS}, () =>
              '<i class="mk-fig__pip"></i>').join('')}</span>
            <span class="mk-quorum__state"></span>
          </div>`).join('')}
        </div>
        <p class="mk-fig__legend" aria-hidden="true">
          <span class="mk-fig__key mk-fig__key--setup"></span>connecting
          <span class="mk-fig__key mk-fig__key--wait"></span>waiting
          <span class="mk-fig__key mk-fig__key--ok"></span>passed
          <span class="mk-fig__key mk-fig__key--down"></span>failed
        </p>
        <div class="mk-quorum__log" aria-hidden="true"></div>
      </div>

      <div class="mk-quorum__ballot">
        <p class="mk-fig__colhead">→ the vote · ${SEATS} reporting</p>
        <ol class="mk-quorum__seats">${Array.from({length: SEATS}, (_, i) => `
          <li class="mk-quorum__seat"><span class="mk-quorum__seatno">${i + 1}</span><span class="mk-quorum__seatname">—</span><span class="mk-quorum__seatmark"></span></li>`).join('')}
          <li class="mk-quorum__bar">
            ${picker('Rule that opens the incident', POLICIES, policy.value)}
            <span class="mk-quorum__barneed"></span>
          </li>
        </ol>
        ${SILENT.map(r => `<p class="mk-quorum__ghost"><span class="mk-quorum__ghostmark">·</span><span class="mk-quorum__ghostname">${r.name}</span><span class="mk-quorum__ghostnote">no data · no vote</span></p>`).join('')}
        <p class="mk-fig__verdict" role="status" aria-live="polite"></p>
      </div>
    </div>

    <p class="mk-fig__caption">Six rounds of a real vote. A region only votes after two failures in a row, and the incident opens once enough votes land. Change the rule to see where that line falls: any region down pages you in round four, a majority holds out until round six, and all regions never pages at all, because sa-east misses one check in round three and passes again on the next pass.</p>`;

  const lanes = [...root.querySelectorAll('.mk-quorum__lane')];
  const seatEls = [...root.querySelectorAll('.mk-quorum__seat')];
  const barEl = root.querySelector('.mk-quorum__bar');
  const barNeedEl = root.querySelector('.mk-quorum__barneed');
  const pickEl = root.querySelector('.mk-fig__pick');
  const needEl = root.querySelector('.mk-fig__need');
  const flow = root.querySelector('.mk-quorum__flow');
  const logEl = root.querySelector('.mk-quorum__log');
  const bigEl = root.querySelector('.mk-fig__big');
  const clockEl = root.querySelector('.mk-fig__clock');
  const checksEl = root.querySelector('.mk-quorum__checks');
  const verdictEl = root.querySelector('.mk-fig__verdict');
  const roundEl = root.querySelector('.mk-fig__round');
  const toggleBtn = root.querySelector('[data-act="toggle"]');
  const prevBtn = root.querySelector('[data-act="prev"]');
  const nextBtn = root.querySelector('[data-act="next"]');

  const cells = lanes.map(l => [...l.querySelectorAll('.mk-quorum__cell')]);
  const {place, park, add} = mover(flow);
  const chips = lanes.map(() => add('mk-fig__chip'));
  const votes = lanes.map(() => add('mk-fig__chip mk-fig__chip--vote'));

  const player = {i: -1, playing: false, seated: 0};
  // Last round whose result has actually landed, per region: the seats follow
  // the probes rather than the tally.
  const seen = REGIONS.map(() => -1);
  const {later, clear: clearTimers} = timers();

  function renderLog(round, upto){
    const lines = [];
    for (let r = 0; r <= round; r++)
      for (let ri = 0; ri < REGIONS.length; ri++){
        if (!REGIONS[ri].ms.length) continue;
        if (r === round && ri >= upto) continue;
        lines.push(logLine(ri, r));
      }
    logEl.textContent = '';
    if (!lines.length){ logEl.appendChild(span('mk-quorum__logwait', 'waiting for the first round of checks')); return; }
    for (const l of lines.slice(-LOG_LINES)){
      const row = document.createElement('p');
      row.className = 'mk-quorum__logline';
      row.append(
        span('mk-quorum__logtime', l.time),
        span('mk-quorum__logname', l.name),
        span(`mk-quorum__logcode ${l.pass ? 'is-ok' : 'is-down'}`, l.code),
        span('mk-quorum__logms', l.ms),
        span('mk-quorum__lognote', l.note));
      logEl.appendChild(row);
    }
  }

  function span(cls, text){
    const el = document.createElement('span');
    el.className = cls;
    el.textContent = text;
    return el;
  }

  // Paints one region's row: the pipe result, its run of failures, its word.
  function paintLane(ri, round){
    const lane = lanes[ri], row = tally(round).rows[ri], tick = REGIONS[ri].ticks[round];
    lane.classList.toggle('is-down', row.down);
    lane.classList.toggle('is-silent', !row.reporting);
    cells[ri].forEach((c, ci) => {
      c.className = 'mk-quorum__cell';
      if (!row.reporting) return;
      c.classList.add(ci < STAGES.length - 1 ? 'is-done' : tick === 'x' ? 'is-fail' : 'is-pass');
    });
    lane.querySelectorAll('.mk-fig__pip').forEach((p, pi) =>
      p.classList.toggle('is-lit', row.reporting && pi < row.run));
    lane.querySelector('.mk-quorum__state').textContent = state(row);
  }

  // Every reporting region holds a seat: down votes stack from the top so the
  // threshold line means something, and the rest sit below still passing. A
  // seat only takes a colour once that region's own check has come back, and
  // it drops to neutral while its vote is in the air.
  function paintSeats(round, landed){
    const down = round < 0 ? [] : ORDER.slice(0, landed);
    const roster = [...down, ...REPORTING.filter(i => !down.includes(i))];
    seatEls.forEach((seat, i) => {
      const ri = roster[i], isDown = i < down.length;
      const flying = !isDown && round >= 0 && CONFIRM_AT[ri] <= round;
      const up = !isDown && !flying && seen[ri] >= 0;
      seat.classList.toggle('is-taken', isDown);
      seat.classList.toggle('is-up', up);
      seat.querySelector('.mk-quorum__seatname').textContent = REGIONS[ri].name;
      seat.querySelector('.mk-quorum__seatmark').textContent = isDown ? 'down' : up ? 'up' : '';
    });
    return down.length;
  }

  function settle(round, down){
    const need = needNow();
    const open = down >= need;
    // Last round, every vote in, and the line still not crossed: the rule has
    // had its whole chance, so the verdict says so rather than "holding".
    const spent = round === ROUNDS - 1 && down >= ORDER.length;
    root.classList.toggle('is-open', open && round >= 0);
    barEl.classList.toggle('is-crossed', open && round >= 0);
    bigEl.textContent = String(down);
    verdictEl.textContent = round < 0
      ? 'No region has a vote yet.'
      : open
        ? `Quorum reached. ${down} of ${SEATS} reporting regions agree, so the incident opens and on-call is paged.`
        : spent
          ? `All ${ROUNDS} rounds are done. ${down} of ${SEATS} regions down is not enough for this rule, which needs ${need}, so no alert is sent.`
          : `Holding. ${down} of ${SEATS} regions are down and the rule needs ${need}, so nobody is paged.`;
  }

  // The threshold line sits under the last seat the rule needs, so picking a
  // different rule moves it rather than redrawing the ballot.
  function applyRule(){
    const need = needNow();
    seatEls[need - 1].after(barEl);
    barNeedEl.textContent = `needs ${need}`;
    needEl.textContent = `/${need}`;
    settle(player.i, player.seated);
    halt();
  }

  // Nothing left to show once the incident is open or the rounds run out.
  const finished = () => player.i >= ROUNDS - 1 || root.classList.contains('is-open');

  // The replay stops the moment the rule fires: that is the whole thing it
  // came to show, and the rounds after it are just aftermath.
  function halt(){
    if (root.classList.contains('is-open')) player.playing = false;
    controls();
  }

  // Stepping past the rule firing stays allowed: the run stops, the ballot does
  // not become unreadable.
  function controls(){
    paintTransport({toggle: toggleBtn, prev: prevBtn, next: nextBtn, round: roundEl},
      {playing: player.playing, done: finished(), round: player.i, rounds: ROUNDS});
  }

  // Full state for a round, with no traffic in the air: used for scrubbing,
  // pausing, and the resting frame.
  function paint(round){
    clearTimers();
    player.i = round;
    chips.concat(votes).forEach(park);
    const shown = Math.max(round, 0);
    lanes.forEach((_, ri) => round < 0 ? resetLane(ri) : paintLane(ri, shown));
    // Scrubbing lands everything at once, so nothing is left in the air.
    seen.fill(round < 0 ? -1 : shown);
    player.seated = round < 0 ? 0 : ORDER.filter(i => CONFIRM_AT[i] <= shown).length;
    const down = paintSeats(round < 0 ? -1 : shown, player.seated);
    settle(round, down);
    clockEl.textContent = clock(shown);
    checksEl.textContent = `${round < 0 ? 0 : SEATS * (round + 1)} checks`;
    renderLog(round, round < 0 ? 0 : REGIONS.length);
    controls();
  }

  function resetLane(ri){
    const lane = lanes[ri];
    lane.classList.remove('is-down');
    lane.classList.toggle('is-silent', !REGIONS[ri].ms.length);
    cells[ri].forEach(c => { c.className = 'mk-quorum__cell'; });
    lane.querySelectorAll('.mk-fig__pip').forEach(p => p.classList.remove('is-lit'));
    lane.querySelector('.mk-quorum__state').textContent = REGIONS[ri].ms.length ? 'idle' : 'no data';
  }

  // One round: every reporting region flies its own check at once, and a
  // region that just confirmed sends a vote on to the ballot.
  function runRound(round){
    player.i = round;
    clockEl.textContent = clock(round);
    controls();
    lanes.forEach((lane, ri) => {
      if (!REGIONS[ri].ms.length) return;
      const c = cells[ri], chip = chips[ri], d = ri * STAGGER_MS;
      c.forEach(x => { x.className = 'mk-quorum__cell'; });
      later(d, () => {
        place(chip, lane.querySelector('.mk-quorum__name'), 0);
        chip.classList.add('is-live');
      });
      // Each stage lights as the chip reaches it; the last one waits for the
      // result the check actually returned.
      let t = d + 30;
      STAGES.forEach((_, ci) => {
        const leave = t;
        later(leave, () => place(chip, c[ci], LEG[ci]));
        t += LEG[ci];
        if (ci < STAGES.length - 1) later(t, () => c[ci].classList.add('is-lit'));
      });
      later(t, () => {
        park(chip);
        land(ri, round);
      });
    });
    later(ROUND_MS, () => {
      if (!player.playing) return;
      if (round >= ROUNDS - 1){ player.playing = false; controls(); return; }
      runRound(round + 1);
    });
  }

  // A check lands: the row updates, the log gains a line, and a region that
  // just crossed the confirmation count sends its vote to the ballot.
  function land(ri, round){
    seen[ri] = round;
    paintLane(ri, round);
    paintSeats(round, player.seated);
    renderLog(round, ri + 1);
    checksEl.textContent = `${SEATS * round + REGIONS.slice(0, ri + 1).filter(r => r.ms.length).length} checks`;

    if (CONFIRM_AT[ri] !== round) return;
    const seatIndex = ORDER.filter(i => CONFIRM_AT[i] <= round).indexOf(ri);
    const seat = seatEls[seatIndex], chip = votes[ri];
    later(120, () => {
      // Leaves from the result that earned it, not from somewhere mid-pipe.
      place(chip, cells[ri].at(-1), 0);
      chip.classList.add('is-live');
      later(20, () => place(chip, seat, LEG_VOTE));
    });
    later(140 + LEG_VOTE, () => {
      park(chip);
      player.seated++;
      settle(round, paintSeats(round, player.seated));
      halt();
    });
  }

  function play(){
    if (finished()) paint(-1);
    player.playing = true;
    if (calm.matches){ player.playing = false; paint(ROUNDS - 1); return; }
    runRound(player.i < 0 ? 0 : player.i);
  }

  function pause(){ player.playing = false; paint(player.i); }

  function step(to){
    player.playing = false;
    paint(Math.min(ROUNDS - 1, Math.max(0, to)));
  }

  toggleBtn.addEventListener('click', () => player.playing ? pause() : play());
  prevBtn.addEventListener('click', () => step(player.i - 1));
  nextBtn.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));
  // Swapping the rule re-reads the current frame rather than restarting: the
  // votes already cast simply meet a different line, and a run in flight keeps
  // going unless the new rule is already satisfied.
  pickEl.addEventListener('change', () => {
    policy = POLICIES.find(p => p.value === pickEl.value);
    applyRule();
  });
  applyRule();
  paint(-1);
}

document.querySelectorAll('.mk-embed-quorum').forEach(mount);
