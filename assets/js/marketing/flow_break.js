import { autoplay, choices, paintTransport, stop, timers, transport, typeOut } from './_figure.js';

/* The same two checks, run against a login the reader gets to break.

   The HTTP panel is the constant: whichever fault is selected, it resolves,
   connects, handshakes, reads a status line and reports up. Only the flow
   moves. Picking a different fault moves where it dies and what the page says
   at that moment, which is the argument the prose can only assert. */

const ROUND_MS = 1500;
const TICKS = 8;
const HOST = 'app.example.com';

// One request, every phase healthy. Identical under every fault, because none
// of these faults is reachable from a status line.
const HTTP_PHASES = [
  {label: 'dns', detail: 'resolve', ms: 6},
  {label: 'tcp', detail: 'connect', ms: 121},
  {label: 'tls', detail: 'handshake', ms: 288},
  {label: 'ttfb', detail: 'first byte', ms: 130},
  {label: '200 OK', detail: 'total', ms: 545},
];

const STEPS = [
  {label: 'start /login', pass: 'the form is there', at: 0.9},
  {label: 'fill #email', pass: 'monitor@example.com', type: true, at: 1.7},
  {label: 'fill #password', pass: '{{login_password}}', type: true, at: 2.5},
  {label: 'click button[type=submit]', pass: 'submitted', load: true, at: 3.8},
  {label: 'assert_url /dashboard', pass: 'signed in', at: 4.4},
];
const ROUNDS = STEPS.length;

/* `at` is the step the flow dies on. Two of these land on the assertion, which
   is not a gap in the example: a fault that leaves the form working surfaces
   only when you check where you ended up, and that is why the assertion is the
   part of the check that does the work. */
const FAULTS = [
  {
    value: 'none', label: 'nothing broken',
    note: 'both checks agree, which is the only time they do',
  },
  {
    value: 'bundle', label: 'a JS chunk 404s',
    note: 'the deploy went out, one hashed chunk never reached the CDN',
    at: 1, why: 'selector not found: #email',
    page: 'the HTML arrived, the form never rendered',
  },
  {
    value: 'botwall', label: 'bot protection challenges',
    note: 'a rule changed and real customers are being challenged',
    at: 1, why: 'selector not found: #email',
    page: 'Checking your browser before you continue',
  },
  {
    value: 'oauth', label: 'the OAuth secret expired',
    note: 'wired up two years ago, nobody diarised the expiry',
    at: 4, why: 'url does not contain /dashboard',
    page: 'invalid_client: client secret has expired',
  },
  {
    value: 'cookie', label: 'the session cookie is dropped',
    note: 'a cookie policy tightened, the app cannot see the session',
    at: 4, why: 'url does not contain /dashboard',
    page: 'the credentials were accepted, the browser is back on /login',
  },
];
const DEFAULT_FAULT = 'oauth';

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  const clocks = timers();
  let fault = FAULTS.find(f => f.value === DEFAULT_FAULT);

  root.classList.add('mk-fig', 'mk-flow');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">break the login yourself</span>
      <span class="mk-fig__note">which fault is live</span>
      <span class="mk-fig__meta"><span class="mk-flow__elapsed">0.0s</span></span>
    </div>

    ${choices('Which fault is live', FAULTS, DEFAULT_FAULT)}

    <p class="mk-flow__aside mk-flow__why"></p>

    <div class="mk-flow__pair">
      <section class="mk-flow__side">
        <h4 class="mk-fig__colhead">HTTP check · GET /login</h4>
        <ol class="mk-flow__steps">
          ${HTTP_PHASES.map((p, i) => `
            <li class="mk-flow__step" data-phase="${i}" data-state="idle">
              <span class="mk-flow__mark" aria-hidden="true"></span>
              <span class="mk-flow__label">${p.label}</span>
              <span class="mk-flow__detail">${p.detail}</span>
              <span class="mk-flow__ms"></span>
            </li>`).join('')}
        </ol>
        <p class="mk-flow__aside">the same five phases, whichever fault you pick</p>
        <p class="mk-flow__badge" data-verdict="http"></p>
      </section>

      <section class="mk-flow__side">
        <h4 class="mk-fig__colhead">Browser flow</h4>

        <div class="mk-flow__bar" data-state="idle">
          <span class="mk-flow__dots" aria-hidden="true"><i></i><i></i><i></i></span>
          <span class="mk-flow__url"><span class="mk-flow__host">${HOST}</span><b class="mk-flow__path">/login</b></span>
          <span class="mk-flow__want">expected /dashboard</span>
          <span class="mk-flow__sweep" aria-hidden="true"></span>
        </div>

        <ol class="mk-flow__steps">
          ${STEPS.map((s, i) => `
            <li class="mk-flow__step" data-step="${i}" data-state="idle">
              <span class="mk-flow__mark" aria-hidden="true"></span>
              <span class="mk-flow__label">${s.label}</span>
              <span class="mk-flow__detail"></span>
            </li>`).join('')}
        </ol>

        <p class="mk-flow__page" hidden></p>
        <p class="mk-flow__badge mk-fig__verdict" data-verdict="flow"></p>
      </section>
    </div>

    <div class="mk-fig__foot">${transport('Replay both checks', `${ROUNDS} steps`)}</div>`;

  const els = {
    toggle: root.querySelector('[data-act="toggle"]'),
    prev: root.querySelector('[data-act="prev"]'),
    next: root.querySelector('[data-act="next"]'),
    round: root.querySelector('.mk-fig__round'),
  };
  const pick = root.querySelector('.mk-fig__opts');
  const httpRows = [...root.querySelectorAll('[data-phase]')];
  const flowRows = [...root.querySelectorAll('[data-step]')];
  const bar = root.querySelector('.mk-flow__bar');
  const httpBadge = root.querySelector('[data-verdict="http"]');
  const flowBadge = root.querySelector('[data-verdict="flow"]');
  const elapsed = root.querySelector('.mk-flow__elapsed');
  const why = root.querySelector('.mk-flow__why');
  const page = root.querySelector('.mk-flow__page');

  const player = {i: -1, playing: false};

  // A fault that never breaks runs the journey to the end; the others stop on
  // the step they break, so the replay is only ever as long as the run.
  const lastStep = () => (fault.at === undefined ? ROUNDS - 1 : fault.at);
  const failing = round => fault.at !== undefined && round === fault.at;

  function runClock(from, to){
    if (!player.playing){ elapsed.textContent = `${to.toFixed(1)}s`; return; }
    for (let i = 1; i <= TICKS; i++){
      clocks.later((ROUND_MS / TICKS) * i, () => {
        elapsed.textContent = `${(from + ((to - from) * i) / TICKS).toFixed(1)}s`;
      });
    }
  }

  function lightPhases(done){
    httpRows.forEach((el, i) => {
      const on = i < done;
      el.dataset.state = on ? 'ok' : 'idle';
      el.querySelector('.mk-flow__ms').textContent = on ? `${HTTP_PHASES[i].ms} ms` : '';
    });
    const settled = done >= HTTP_PHASES.length;
    httpBadge.dataset.state = settled ? 'ok' : '';
    httpBadge.textContent = settled ? 'UP' : '';
  }

  function paint(round){
    const was = player.i;
    const end = lastStep();
    player.i = round;
    clocks.clear();

    why.textContent = fault.note;

    // The whole request is over inside the flow's first step, so the phases
    // stream through that one round and then sit there being right about
    // nothing that matters.
    if (round < 0) lightPhases(0);
    else if (round === 0 && player.playing){
      lightPhases(0);
      const beat = ROUND_MS / (HTTP_PHASES.length + 1);
      HTTP_PHASES.forEach((_, i) => clocks.later(beat * (i + 1), () => lightPhases(i + 1)));
    } else lightPhases(HTTP_PHASES.length);

    flowRows.forEach((el, i) => {
      const step = STEPS[i];
      const detail = el.querySelector('.mk-flow__detail');
      const state = round < 0 || i > round ? 'idle'
        : i < round ? 'ok'
        : failing(i) ? 'down' : 'run';
      el.dataset.state = state;
      if (state === 'idle'){ detail.textContent = ''; return; }
      if (state === 'down'){ detail.textContent = fault.why; return; }
      if (state === 'run' && step.type && player.playing) typeOut(clocks, detail, step.pass, ROUND_MS * 0.5);
      else detail.textContent = step.pass;
    });

    const broke = round >= 0 && failing(round);
    const submitting = round >= 0 && STEPS[round].load && player.playing && !broke;
    bar.dataset.state = round < 0 ? 'idle' : submitting ? 'load' : broke ? 'stuck' : 'live';

    page.hidden = !broke;
    page.textContent = broke ? `page said: ${fault.page}` : '';

    const finished = round >= end;
    const passed = finished && fault.at === undefined;
    flowBadge.dataset.state = broke ? 'down' : passed ? 'ok' : '';
    flowBadge.textContent = broke
      ? `DOWN · step ${round + 1}/${ROUNDS} ${STEPS[round].label.split(' ')[0]}`
      : passed ? 'UP' : round < 0 ? '' : 'signing in…';
    root.classList.toggle('is-open', broke);

    const to = round < 0 ? 0 : STEPS[round].at;
    if (finished) elapsed.textContent = `${to.toFixed(1)}s`;
    else runClock(was < 0 || was >= round ? Math.max(0, to - 0.7) : STEPS[was].at, to);

    paintTransport(els, {playing: player.playing, done: finished, round, rounds: end + 1, unit: 'step'});
  }

  function runFrom(round){
    paint(round);
    // Repainting here would clear the timers the frame is still running on.
    if (!player.playing || round >= lastStep()) return stop(player, els, {round, rounds: lastStep() + 1, unit: 'step'});
    clocks.later(ROUND_MS, () => runFrom(round + 1));
  }

  function play(){
    clocks.clear();
    if (player.i >= lastStep()) paint(-1);
    player.playing = true;
    if (calm.matches){ player.playing = false; paint(lastStep()); return; }
    runFrom(player.i + 1);
  }

  function pause(){ player.playing = false; clocks.clear(); paint(player.i); }

  function step(to){
    player.playing = false;
    clocks.clear();
    paint(Math.min(lastStep(), Math.max(0, to)));
  }

  // Switching fault mid-run replays from the top: the interesting frame is the
  // one where this fault dies, and it is rarely the frame already on screen.
  pick.addEventListener('change', e => {
    fault = FAULTS.find(f => f.value === e.target.value) || FAULTS[0];
    clocks.clear();
    player.playing = false;
    paint(-1);
    if (!calm.matches) play();
  });

  els.toggle.addEventListener('click', () => player.playing ? pause() : play());
  els.prev.addEventListener('click', () => step(player.i - 1));
  els.next.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-flow-break').forEach(mount);
