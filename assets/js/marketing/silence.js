import { autoplay, mover, paintTransport, picker, timers, transport } from './_figure.js';

/* What a quiet region does to the vote.

   Regions push their results to the control plane; nothing is ever polled. So
   the figure animates arrivals, and a region that lost its link simply sends
   nothing. The only question left is what that silence means, which is the
   control the reader gets to turn: a region that stops reporting either leaves
   the vote or is read as a down vote, and the same eight cycles end either in
   silence or in a page for an outage that never happened. */

const START_MIN = 10 * 60 + 20;
// Five-minute checks on purpose: the vote reads a window of
// `interval * 2 * confirmations` under a ten-minute floor, so a one-minute
// monitor would take ten cycles of silence to age a region out and the figure
// could not show the drop. Shortening this interval makes WINDOW_CYCLES a lie.
const STEP_MIN = 5;
const CONFIRMATIONS = 2;
// Results older than this are outside the window the vote reads, so the region
// holding them is no longer in the vote at all.
const WINDOW_CYCLES = 2 * CONFIRMATIONS;
const ROUND_MS = 1600;
const STAGGER_MS = 70;
const HOP = 440;                                   // region → the control plane

const REGIONS = [
  {name: 'eu-helsinki', ticks: 'oooooooo'},
  {name: 'us-east',     ticks: 'ooxxxxxx'},
  {name: 'apac-sg',     ticks: 'ooxxxxxx'},
  {name: 'sa-east',     ticks: 'oooooooo'},
  {name: 'eu-west',     ticks: 'oo------'},        // the link drops, nothing arrives
];
const ROUNDS = REGIONS[0].ticks.length;

// The two readings of silence: the one the vote uses, and the one that turns a
// monitoring outage into an incident on your side.
const READINGS = [
  {value: 'out',  label: 'leaves the vote',       silentIsDown: false},
  {value: 'down', label: 'counts as a down vote', silentIsDown: true},
];
const DEFAULT_READING = 'out';

const clock = round => {
  const m = START_MIN + round * STEP_MIN;
  return `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`;
};

const lastHeard = (ticks, round) => {
  for (let i = round; i >= 0; i--) if (ticks[i] !== '-') return i;
  return -1;
};

const QUIET = REGIONS.findIndex(r => r.ticks.includes('-'));

// One region's standing after `round`: heard from recently enough to be in the
// vote at all, and if so whether its own run of failures makes it a down vote.
function standing(ri, round, reading){
  // Nothing has run yet, which is not the same thing as a region going quiet.
  if (round < 0) return {heard: -1, vote: 'none'};
  const ticks = REGIONS[ri].ticks;
  const heard = lastHeard(ticks, round);
  if (heard < 0 || round - heard > WINDOW_CYCLES)
    return {heard, vote: reading.silentIsDown ? 'down' : 'none'};
  let run = 0;
  for (let i = heard; i >= 0 && ticks[i] === 'x'; i--) run++;
  return {heard, vote: run >= CONFIRMATIONS ? 'down' : 'up'};
}

// The vote over whoever is still in it, read from the cycle each region has
// actually landed rather than the cycle the player is on.
function tally(rounds, reading){
  const rows = REGIONS.map((_, ri) => standing(ri, rounds[ri], reading));
  const reporting = rows.filter(r => r.vote !== 'none').length;
  return {reporting, down: rows.filter(r => r.vote === 'down').length,
          need: Math.floor(reporting / 2) + 1};
}

const VOTE_WORD = {up: 'up', down: 'down', none: 'no vote'};

// How old a region's last result is. The age only shows once it starts to
// matter, so the number climbing towards the window is the thing you notice.
function heardText(heard, round){
  if (heard < 0) return 'never';
  const age = (round - heard) * STEP_MIN;
  return age ? `${clock(heard)} · ${age}m` : clock(heard);
}

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  let reading = READINGS.find(r => r.value === DEFAULT_READING);

  root.classList.add('mk-fig', 'mk-silence');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">control plane</span>
      <span class="mk-fig__note">a region that stops reporting ${picker('What silence means to the vote', READINGS, reading.value)}</span>
      <span class="mk-fig__meta"><b class="mk-fig__clock">${clock(0)}</b> · <span class="mk-silence__window">window ${WINDOW_CYCLES * STEP_MIN} min</span></span>
    </div>

    <div class="mk-fig__lead">
      <p class="mk-fig__score">
        <span class="mk-fig__big">0</span><span class="mk-fig__need">/0</span>
        <span class="mk-fig__unit"></span>
      </p>
      ${transport('Cycle playback', `${ROUNDS} cycles`)}
    </div>

    <div class="mk-silence__flow">
      <div class="mk-silence__probes">
        <p class="mk-fig__colhead">every region pushes its result · nothing is polled</p>
        <div class="mk-silence__lanes">${REGIONS.map((r, ri) => `
          <div class="mk-silence__lane" data-lane="${ri}">
            <span class="mk-silence__name">${r.name}</span>
            <span class="mk-silence__ticks" aria-hidden="true">${r.ticks.split('').map(() =>
              '<i class="mk-silence__tick"></i>').join('')}</span>
            <span class="mk-silence__heard"></span>
            <span class="mk-silence__vote"></span>
          </div>`).join('')}
        </div>
        <p class="mk-fig__legend" aria-hidden="true">
          <span class="mk-fig__key mk-fig__key--ok"></span>passed
          <span class="mk-fig__key mk-fig__key--down"></span>failed
          <span class="mk-fig__key mk-fig__key--idle"></span>nothing arrived
        </p>
      </div>

      <div class="mk-silence__side">
        <p class="mk-fig__colhead">→ what the brain counts</p>
        <div class="mk-silence__brain">
          <p class="mk-silence__stat"><span>in the vote</span><b class="mk-silence__reporting">0</b></p>
          <p class="mk-silence__stat"><span>down votes</span><b class="mk-silence__down">0</b></p>
          <p class="mk-silence__stat mk-silence__stat--rule"><span>majority</span><b class="mk-silence__need">0</b></p>
        </div>
        <p class="mk-fig__verdict" role="status" aria-live="polite"></p>
      </div>
    </div>

    <p class="mk-fig__caption">Eight cycles where two regions really do fail and a third loses its link to the control plane. Silence is not a vote, so the majority is counted over the regions that still speak and nobody is paged. Read that silence as a down vote instead and the same eight cycles page you for an outage your site never had.</p>`;

  const lanes = [...root.querySelectorAll('.mk-silence__lane')];
  const flow = root.querySelector('.mk-silence__flow');
  const brainEl = root.querySelector('.mk-silence__brain');
  const pickEl = root.querySelector('.mk-fig__pick');
  const bigEl = root.querySelector('.mk-fig__big');
  const needSlot = root.querySelector('.mk-fig__need');
  const unitEl = root.querySelector('.mk-fig__unit');
  const clockEl = root.querySelector('.mk-fig__clock');
  const reportingEl = root.querySelector('.mk-silence__reporting');
  const downEl = root.querySelector('.mk-silence__down');
  const needEl = root.querySelector('.mk-silence__need');
  const verdictEl = root.querySelector('.mk-fig__verdict');
  const roundEl = root.querySelector('.mk-fig__round');
  const toggleBtn = root.querySelector('[data-act="toggle"]');
  const prevBtn = root.querySelector('[data-act="prev"]');
  const nextBtn = root.querySelector('[data-act="next"]');

  const ticks = lanes.map(l => [...l.querySelectorAll('.mk-silence__tick')]);
  const {place, park, add} = mover(flow);
  const chips = lanes.map(() => add('mk-fig__chip'));

  const player = {i: -1, playing: false};
  // Last cycle that has actually landed, per region, so the brain counts what
  // is on screen rather than what the round says.
  const landed = REGIONS.map(() => -1);
  const {later, clear: clearTimers} = timers();

  // The result a region recorded for one cycle, in its own row. A gap is drawn
  // as an open slot: nothing arrived is not the same as something bad.
  function markTick(ri, round){
    const t = ticks[ri][round], result = REGIONS[ri].ticks[round];
    t.className = 'mk-silence__tick';
    t.classList.add(result === '-' ? 'is-missing' : result === 'x' ? 'is-fail' : 'is-pass');
  }

  // Paints one region's row: what arrived, when it was last heard, how it votes.
  function paintLane(ri, round){
    const lane = lanes[ri];
    const {heard, vote} = round < 0 ? {heard: -1, vote: null} : standing(ri, round, reading);
    ticks[ri].forEach((t, ti) => {
      if (ti > round) t.className = 'mk-silence__tick';
      else markTick(ri, ti);
    });
    lane.classList.toggle('is-out', vote === 'none');
    lane.querySelector('.mk-silence__heard').textContent = round < 0 ? '' : heardText(heard, round);
    lane.querySelector('.mk-silence__vote').textContent = round < 0 ? 'idle' : VOTE_WORD[vote];
  }

  function verdict(round, t){
    const quiet = REGIONS[QUIET].name;
    const since = clock(lastHeard(REGIONS[QUIET].ticks, ROUNDS - 1));
    if (round < 0)
      return `Every region pushes its own results in. The vote counts whatever arrived in the last ${WINDOW_CYCLES} cycles, so a region that cannot reach the control plane sends nothing at all.`;
    if (t.down >= t.need)
      return `Quorum reached, on a monitoring outage. ${quiet} has not been heard from since ${since}, and reading that silence as a down vote makes ${t.down} of ${t.reporting}. Your site never had ${t.down} regions failing: one of them just could not reach the control plane.`;
    if (round < ROUNDS - 1)
      return `Holding. ${t.down} of ${t.reporting} regions are down and a majority is ${t.need}, so nobody is paged.`;
    return `All ${ROUNDS} cycles are in. ${quiet} went quiet after ${since} and left the vote once its last result aged out, so the majority is counted over the ${t.reporting} regions that still speak: ${t.down} down against a majority of ${t.need}, and nobody is paged.`;
  }

  function settle(round){
    // At rest the roster is whole: no region has gone quiet, there is just no
    // data yet.
    const t = round < 0
      ? {reporting: REGIONS.length, down: 0, need: Math.floor(REGIONS.length / 2) + 1}
      : tally(landed, reading);
    const open = t.down >= t.need && round >= 0;
    root.classList.toggle('is-open', open);
    brainEl.classList.toggle('is-open', open);
    bigEl.textContent = String(t.down);
    needSlot.textContent = `/${t.need}`;
    unitEl.textContent = `down votes of the ${t.reporting} still in the vote`;
    reportingEl.textContent = String(t.reporting);
    downEl.textContent = String(t.down);
    needEl.textContent = String(t.need);
    verdictEl.textContent = verdict(round, t);
  }

  function controls(){
    paintTransport({toggle: toggleBtn, prev: prevBtn, next: nextBtn, round: roundEl},
      {playing: player.playing, done: player.i >= ROUNDS - 1, round: player.i, rounds: ROUNDS, unit: 'cycle'});
  }

  // Full state for a cycle, with nothing in the air.
  function paint(round){
    clearTimers();
    player.i = round;
    chips.forEach(park);
    landed.fill(round);
    lanes.forEach((_, ri) => paintLane(ri, round));
    settle(round);
    clockEl.textContent = clock(Math.max(round, 0));
    controls();
  }

  // One cycle: every region that still has a link pushes its result in.
  function runRound(round){
    player.i = round;
    clockEl.textContent = clock(round);
    controls();
    lanes.forEach((_, ri) => {
      const result = REGIONS[ri].ticks[round];
      const d = ri * STAGGER_MS, chip = chips[ri];
      later(d, () => {
        // The region records its own result first; only then does it push. A
        // region with no link records nothing and sends nothing.
        markTick(ri, round);
        if (result === '-') return;
        chip.classList.toggle('is-fail', result === 'x');
        place(chip, ticks[ri][round], 0);
        chip.classList.add('is-live');
        // Lands where the result actually counts. Every arrival proves the
        // region is in the vote; it only reaches the down votes once it is the
        // second failure in a row, which is the confirmation gate.
        const down = standing(ri, round, reading).vote === 'down';
        later(20, () => place(chip, down ? downEl : reportingEl, HOP));
      });
      later(d + HOP + 40, () => {
        park(chip);
        landed[ri] = round;
        paintLane(ri, round);
        settle(round);
      });
    });
    later(ROUND_MS, () => {
      if (!player.playing) return;
      if (round >= ROUNDS - 1){ player.playing = false; controls(); return; }
      runRound(round + 1);
    });
  }

  function play(){
    if (player.i >= ROUNDS - 1) paint(-1);
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
  // Same eight cycles either way: only the meaning of the gap changes.
  pickEl.addEventListener('change', () => {
    reading = READINGS.find(r => r.value === pickEl.value);
    lanes.forEach((_, ri) => paintLane(ri, landed[ri]));
    settle(player.i);
  });

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-silence').forEach(mount);
