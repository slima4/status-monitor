import { autoplay, paintTransport, stop, timers, transport, typeOut } from './_figure.js';

/* Two checks against the same login, replayed side by side.

   The HTTP check times every phase of its one request and is finished before
   the flow has filled a field. The flow keeps going, and the step that fails is
   the one no status code could have reached. The point is the last frame: same
   target, same minute, opposite verdicts. */

const ROUND_MS = 1700;
const HOST = 'the-internet.herokuapp.com';

// Per-phase legs, the way the probe records them, measured against the host.
const HTTP_PHASES = [
  {label: 'dns', detail: 'resolve', ms: 5},
  {label: 'tcp', detail: 'connect', ms: 140},
  {label: 'tls', detail: 'handshake', ms: 302},
  {label: 'ttfb', detail: 'first byte', ms: 144},
  {label: '200 OK', detail: 'total', ms: 592},
];

const FLOW_STEPS = [
  {label: 'open /login', detail: 'the form is there', at: 0.8},
  {label: 'fill #username', detail: 'tomsmith', type: true, at: 1.6},
  {label: 'fill #password', detail: '{{login_password}}', type: true, at: 2.4},
  {label: 'click #login > button', detail: 'submitted', load: true, at: 3.7},
  {label: 'assert url /secure', detail: 'the browser is still on /login', state: 'down', at: 4.3},
];

const ROUNDS = FLOW_STEPS.length;
const TICKS = 8;

function mount(root){
  const calm = matchMedia('(prefers-reduced-motion:reduce)');
  const clocks = timers();

  root.classList.add('mk-fig', 'mk-flow');
  root.innerHTML = `
    <div class="mk-fig__head">
      <span class="mk-fig__tag">same login, same minute</span>
      <span class="mk-fig__note">the password stopped being accepted an hour ago</span>
      <span class="mk-fig__meta"><span class="mk-flow__elapsed">0.0s</span></span>
    </div>

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
        <p class="mk-flow__aside">every phase timed, every one of them healthy, and not one of them touches the form</p>
        <p class="mk-flow__badge" data-verdict="http"></p>
      </section>

      <section class="mk-flow__side">
        <h4 class="mk-fig__colhead">Browser flow</h4>

        <div class="mk-flow__bar" data-state="idle">
          <span class="mk-flow__dots" aria-hidden="true"><i></i><i></i><i></i></span>
          <span class="mk-flow__url"><span class="mk-flow__host">${HOST}</span><b class="mk-flow__path">/login</b></span>
          <span class="mk-flow__want">expected /secure</span>
          <span class="mk-flow__sweep" aria-hidden="true"></span>
        </div>

        <ol class="mk-flow__steps">
          ${FLOW_STEPS.map((s, i) => `
            <li class="mk-flow__step" data-step="${i}" data-state="idle">
              <span class="mk-flow__mark" aria-hidden="true"></span>
              <span class="mk-flow__label">${s.label}</span>
              <span class="mk-flow__detail"></span>
            </li>`).join('')}
        </ol>

        <p class="mk-flow__badge mk-fig__verdict" data-verdict="flow"></p>
      </section>
    </div>

    <div class="mk-fig__foot">${transport('Replay the two checks', `${ROUNDS} steps`)}</div>`;

  const els = {
    toggle: root.querySelector('[data-act="toggle"]'),
    prev: root.querySelector('[data-act="prev"]'),
    next: root.querySelector('[data-act="next"]'),
    round: root.querySelector('.mk-fig__round'),
  };
  const httpRows = [...root.querySelectorAll('[data-phase]')];
  const flowRows = [...root.querySelectorAll('[data-step]')];
  const bar = root.querySelector('.mk-flow__bar');
  const httpBadge = root.querySelector('[data-verdict="http"]');
  const flowBadge = root.querySelector('[data-verdict="flow"]');
  const elapsed = root.querySelector('.mk-flow__elapsed');

  const player = {i: -1, playing: false};

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
    player.i = round;
    clocks.clear();

    // The whole HTTP check is over inside the flow's first step, so its phases
    // stream through that one round and then sit there being wrong.
    if (round < 0) lightPhases(0);
    else if (round === 0 && player.playing){
      lightPhases(0);
      const beat = ROUND_MS / (HTTP_PHASES.length + 1);
      HTTP_PHASES.forEach((_, i) => clocks.later(beat * (i + 1), () => lightPhases(i + 1)));
    } else lightPhases(HTTP_PHASES.length);

    flowRows.forEach((el, i) => {
      const step = FLOW_STEPS[i];
      const detail = el.querySelector('.mk-flow__detail');
      const state = round < 0 || i > round ? 'idle'
        : i < round ? 'ok'
        : step.state === 'down' ? 'down' : 'run';
      el.dataset.state = state;
      if (state === 'idle'){ detail.textContent = ''; return; }
      if (state === 'run' && step.type && player.playing) typeOut(clocks, detail, step.detail, ROUND_MS * 0.5);
      else detail.textContent = step.detail;
    });

    const submitting = round >= 0 && FLOW_STEPS[round].load && player.playing;
    const failed = round >= 0 && FLOW_STEPS[round].state === 'down';
    bar.dataset.state = round < 0 ? 'idle' : submitting ? 'load' : failed ? 'stuck' : 'live';

    flowBadge.dataset.state = failed ? 'down' : '';
    flowBadge.textContent = failed
      ? `DOWN · step ${round + 1}/${ROUNDS} assert_url`
      : round < 0 ? '' : 'signing in…';
    root.classList.toggle('is-open', failed);

    const to = round < 0 ? 0 : FLOW_STEPS[round].at;
    // The last step ends the run, so the clock lands with the verdict.
    if (round >= ROUNDS - 1) elapsed.textContent = `${to.toFixed(1)}s`;
    else runClock(was < 0 || was >= round ? Math.max(0, to - 0.7) : FLOW_STEPS[was].at, to);

    paintTransport(els, {playing: player.playing, done: round >= ROUNDS - 1, round, rounds: ROUNDS, unit: 'step'});
  }

  function runFrom(round){
    paint(round);
    // Repainting the frame here would clear the timers it is still running on.
    if (!player.playing || round >= ROUNDS - 1) return stop(player, els, {round, rounds: ROUNDS, unit: 'step'});
    clocks.later(ROUND_MS, () => runFrom(round + 1));
  }

  // Resuming picks up at the next step; the one on screen has already played.
  function play(){
    clocks.clear();
    if (player.i >= ROUNDS - 1) paint(-1);
    player.playing = true;
    if (calm.matches){ player.playing = false; paint(ROUNDS - 1); return; }
    runFrom(player.i + 1);
  }

  function pause(){ player.playing = false; clocks.clear(); paint(player.i); }

  function step(to){
    player.playing = false;
    clocks.clear();
    paint(Math.min(ROUNDS - 1, Math.max(0, to)));
  }

  els.toggle.addEventListener('click', () => player.playing ? pause() : play());
  els.prev.addEventListener('click', () => step(player.i - 1));
  els.next.addEventListener('click', () => step(player.i < 0 ? 0 : player.i + 1));

  paint(-1);
  autoplay(root, play);
}

document.querySelectorAll('.mk-embed-flow-gap').forEach(mount);
