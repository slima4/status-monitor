import { autoplay, paintTransport, picker, timers, transport } from './_figure.js';

/* The same dying job, watched two ways.

   Error-based alerting can only report what the job produced, so a job that
   produced nothing gives it nothing to report. The heartbeat watches the gap
   instead. Six ways a scheduled job goes wrong, and the reader picks which one
   killed it: the left column stays quiet in all six, and the right one catches
   four. It does not catch the two where the job exits 0 and pings on its way
   out, which is the honest limit of the pattern and is drawn as such. */

const START_MIN = 3 * 60;                          // 03:00, the last good run
const STEP_MIN = 5;
const ROUNDS = 12;
const PERIOD_MIN = 30;
const GRACE_MIN = 15;
const ROUND_MS = 900;

// Named the same way as the post's six headings, so a reader moving between
// them does not have to translate.
const MODES = [
  {
    value: 'never',
    label: 'never started',
    pings: [0],
    lines: [],
    blind: 'Nothing ran, so nothing was written anywhere.',
  },
  {
    value: 'died',
    label: 'started and died halfway',
    pings: [0],
    lines: [{at: 6, text: 'dmesg: Out of memory: killed process 2841 (backup.sh)'}],
    blind: 'A kernel line nobody is paging on, in a log nobody is reading at 03:30.',
  },
  {
    value: 'empty',
    label: 'finished and did nothing',
    pings: [0, 6],
    lines: [{at: 6, text: 'backup.sh: exit 0'}],
    blind: 'Exit 0 on an empty dump. Every check agrees it worked.',
  },
  {
    value: 'twice',
    label: 'ran twice',
    pings: [0, 6, 6],
    lines: [{at: 6, text: 'backup.sh: exit 0'}, {at: 6, text: 'backup.sh: exit 0'}],
    blind: 'Two successes. Nothing here says there should have been one.',
  },
  {
    value: 'late',
    label: 'ran, but hours late',
    pings: [0, 11],
    lines: [{at: 11, text: 'backup.sh: exit 0'}],
    blind: 'It succeeded in the end, so there is nothing to report at all.',
  },
  {
    value: 'dropped',
    label: 'failed and the alert was dropped',
    pings: [0],
    lines: [{at: 6, text: 'cron: MAILTO delivery failed (rejected by receiver)'}],
    blind: 'The alert fired into a mail path that dropped it. Same as silence.',
  },
];
const DEFAULT_MODE = 'never';

const clock = round => {
  const m = START_MIN + round * STEP_MIN;
  return `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`;
};

const lastPing = (mode, round) => {
  let seen = -1;
  for (const p of mode.pings) if (p <= round) seen = Math.max(seen, p);
  return seen;
};

/** Where the monitor stands after `round`, from the gap since the last ping. */
function state(mode, round){
  if (round < 0) return {gap: 0, word: 'up'};
  const seen = lastPing(mode, round);
  const gap = seen < 0 ? (round + 1) * STEP_MIN : (round - seen) * STEP_MIN;
  const word = gap >= PERIOD_MIN + GRACE_MIN ? 'down' : gap >= PERIOD_MIN ? 'late' : 'up';
  return {gap, word};
}

/** The first round the gap grows past the down threshold, or -1 if it never does. */
const downAt = mode => Array.from({length: ROUNDS}, (_, r) => state(mode, r).word).indexOf('down');
/** Whether the heartbeat ever calls the job down across the whole replay. */
const caught = mode => downAt(mode) >= 0;

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  let mode = MODES.find(m => m.value === DEFAULT_MODE);

  root.classList.add('mk-fig', 'mk-blind');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">03:00 backup</span>
      <span class="mk-fig__note">the job ${picker('Which failure mode killed the job', MODES, mode.value)}</span>
      <span class="mk-fig__meta"><b class="mk-fig__clock">${clock(0)}</b> · every ${PERIOD_MIN}m, ${GRACE_MIN}m grace</span>
    </div>

    <div class="mk-fig__lead">
      <p class="mk-fig__score">
        <span class="mk-fig__big">0</span><span class="mk-fig__need">m</span>
        <span class="mk-fig__unit">since the last ping</span>
      </p>
      ${transport('Timeline playback', `${ROUNDS} steps`)}
    </div>

    <div class="mk-blind__flow">
      <div class="mk-blind__col">
        <p class="mk-fig__colhead">error-based alerting</p>
        <div class="mk-blind__log"></div>
        <p class="mk-blind__tally"><span>would page you</span><b>0</b></p>
      </div>

      <div class="mk-blind__col">
        <p class="mk-fig__colhead">heartbeat check</p>
        <div class="mk-blind__track">
          <span class="mk-blind__ticks" aria-hidden="true">${Array.from({length: ROUNDS}, () =>
            '<i class="mk-blind__tick"></i>').join('')}</span>
          <span class="mk-blind__marks" aria-hidden="true">
            <i class="mk-blind__mark mk-blind__mark--due"><b>due</b></i>
            <i class="mk-blind__mark mk-blind__mark--down"><b>down</b></i>
          </span>
        </div>
        <p class="mk-blind__tally"><span>monitor says</span><b class="mk-blind__word">up</b></p>
        <p class="mk-fig__legend" aria-hidden="true">
          <span class="mk-fig__key mk-fig__key--ok"></span>ping arrived
          <span class="mk-fig__key mk-fig__key--idle"></span>silence
          <span class="mk-fig__key mk-fig__key--late"></span>late
          <span class="mk-fig__key mk-fig__key--down"></span>down
        </p>
      </div>
    </div>

    <p class="mk-fig__verdict" role="status" aria-live="polite"></p>
    <p class="mk-fig__caption">One job, one night, six ways for it to go wrong. The left column can only report what the job produced, so it stays quiet through every one of them. The heartbeat watches the gap instead and catches four. It misses the two where the script exits 0 and pings on its way out, which is worth knowing before you trust it with a backup.</p>`;

  const logEl = root.querySelector('.mk-blind__log');
  const wordEl = root.querySelector('.mk-blind__word');
  const ticks = [...root.querySelectorAll('.mk-blind__tick')];
  const dueEl = root.querySelector('.mk-blind__mark--due');
  const downEl = root.querySelector('.mk-blind__mark--down');
  const pickEl = root.querySelector('.mk-fig__pick');
  const bigEl = root.querySelector('.mk-fig__big');
  const unitEl = root.querySelector('.mk-fig__unit');
  const clockEl = root.querySelector('.mk-fig__clock');
  const verdictEl = root.querySelector('.mk-fig__verdict');
  const roundEl = root.querySelector('.mk-fig__round');
  const toggleBtn = root.querySelector('[data-act="toggle"]');
  const prevBtn = root.querySelector('[data-act="prev"]');
  const nextBtn = root.querySelector('[data-act="next"]');

  const player = {i: -1, playing: false};
  const {later, clear: clearTimers} = timers();

  // The due and down instants are fixed by period and grace, so they sit on the
  // track once rather than being redrawn per frame.
  root.style.setProperty('--mk-steps', String(ROUNDS));
  dueEl.style.setProperty('--at', String(PERIOD_MIN / STEP_MIN));
  downEl.style.setProperty('--at', String((PERIOD_MIN + GRACE_MIN) / STEP_MIN));

  function paintLog(round){
    const shown = mode.lines.filter(l => l.at <= round);
    logEl.innerHTML = shown.length
      ? shown.map(l => `<p class="mk-blind__line"><span>${clock(l.at)}</span>${l.text}</p>`).join('')
      : `<p class="mk-blind__line is-empty">${round < 0 ? 'waiting' : 'nothing'}</p>`;
  }

  function paintTrack(round){
    ticks.forEach((t, i) => {
      t.className = 'mk-blind__tick';
      if (i > round) return;
      if (mode.pings.includes(i)) return t.classList.add('is-ping');
      const {word} = state(mode, i);
      t.classList.add(word === 'up' ? 'is-quiet' : word === 'late' ? 'is-late' : 'is-over');
    });
  }

  function verdict(round){
    const {word} = state(mode, round);
    const down = downAt(mode);
    if (round < 0)
      return `A job that runs every ${PERIOD_MIN} minutes, last seen at ${clock(0)}. Pick what goes wrong with it and watch what each side of the card finds out.`;
    if (round < ROUNDS - 1) return mode.blind;
    if (down < 0)
      return `${mode.blind} The heartbeat is green too: the script exited 0 and pinged on its way out, so the gap never opened. This is the case a heartbeat cannot see, and the reason to check what the job produced as well as that it ran.`;
    // A job that recovers ends the replay green, so the verdict has to carry
    // the outage the last frame no longer shows.
    if (word === 'up')
      return `${mode.blind} The heartbeat still caught it: down at ${clock(down)}, and green again only when the job finally reported at ${clock(round)}. Nothing in the left column recorded that gap at all.`;
    return `${mode.blind} The heartbeat went ${word} at ${clock(down)}, ${PERIOD_MIN + GRACE_MIN} minutes after the last ping, without needing the job to report anything at all.`;
  }

  function settle(round){
    const {gap, word} = state(mode, round);
    const open = word === 'down';
    root.classList.toggle('is-open', open);
    bigEl.textContent = round < 0 ? '0' : String(gap);
    unitEl.textContent = round < 0 ? 'since the last ping' : `since the last ping · down at ${PERIOD_MIN + GRACE_MIN}m`;
    wordEl.textContent = round < 0 ? 'up' : word;
    wordEl.className = `mk-blind__word is-${round < 0 ? 'up' : word}`;
    verdictEl.textContent = verdict(round);
  }

  function controls(){
    paintTransport({toggle: toggleBtn, prev: prevBtn, next: nextBtn, round: roundEl},
      {playing: player.playing, done: player.i >= ROUNDS - 1, round: player.i, rounds: ROUNDS, unit: 'step'});
  }

  function paint(round){
    clearTimers();
    player.i = round;
    root.classList.toggle('is-blindspot', !caught(mode));
    paintLog(round);
    paintTrack(round);
    settle(round);
    clockEl.textContent = clock(Math.max(round, 0));
    controls();
  }

  function runRound(round){
    paint(round);
    player.playing = true;
    controls();
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
  // Same night either way: only what went wrong with the job changes.
  pickEl.addEventListener('change', () => {
    mode = MODES.find(m => m.value === pickEl.value);
    // paint() drops the queued step, so the transport has to stop calling itself playing.
    player.playing = false;
    paint(player.i);
  });

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-blind').forEach(mount);
