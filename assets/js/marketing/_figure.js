/* Shared parts of the replayable figures in the blog: the transport, the chip
   that travels, and the timers that drive both. Each figure owns its own data
   and layout; this keeps the chrome from drifting between them. */

const ICONS = {
  prev: '<path d="M18 5v14L8 12z"/><rect x="5" y="5" width="2.2" height="14" rx="1"/>',
  play: '<path d="M7 4.5v15l13-7.5z"/>',
  pause: '<rect x="6.5" y="4.5" width="4" height="15" rx="1"/><rect x="13.5" y="4.5" width="4" height="15" rx="1"/>',
  replay: '<path class="mk-fig__stroke" d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path class="mk-fig__stroke" d="M3 3v5h5"/>',
  next: '<path d="M6 5v14l10-7z"/><rect x="16.8" y="5" width="2.2" height="14" rx="1"/>',
};

const icon = (name, cls) => `<svg class="${cls}" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`;

/** Prev / play / next, plus the round counter that names where the replay is. */
export function transport(label, rest){
  return `
    <div class="mk-fig__tp" role="group" aria-label="${label}">
      <button type="button" class="mk-fig__btn" data-act="prev" aria-label="Previous round" title="Previous round">${icon('prev', '')}</button>
      <button type="button" class="mk-fig__btn mk-fig__btn--go" data-act="toggle" data-state="play">
        ${icon('play', 'mk-fig__ic mk-fig__ic--play')}
        ${icon('pause', 'mk-fig__ic mk-fig__ic--pause')}
        ${icon('replay', 'mk-fig__ic mk-fig__ic--replay')}
      </button>
      <button type="button" class="mk-fig__btn" data-act="next" aria-label="Next round" title="Next round">${icon('next', '')}</button>
      <span class="mk-fig__round">${rest}</span>
    </div>`;
}

/** A dropdown that mirrors one of the monitor's own settings. */
export function picker(label, options, selected){
  return `
    <span class="mk-fig__pickwrap">
      <select class="mk-fig__pick" aria-label="${label}">${options.map(o =>
        `<option value="${o.value}"${o.value === selected ? ' selected' : ''}>${o.label}</option>`).join('')}</select>
    </span>`;
}

/** Timers the figure can drop all at once when the viewer scrubs or pauses. */
export function timers(){
  const pending = [];
  return {
    later: (ms, fn) => pending.push(setTimeout(fn, ms)),
    clear: () => { for (const t of pending) clearTimeout(t); pending.length = 0; },
  };
}

/** Moves a chip between elements of `host`, in the host's own coordinates. */
export function mover(host){
  return {
    place(chip, target, ms){
      const h = host.getBoundingClientRect(), r = target.getBoundingClientRect();
      chip.style.transition = ms ? `transform ${ms}ms cubic-bezier(.2,0,0,1)` : 'none';
      chip.style.transform = `translate(${r.left - h.left + r.width / 2 - 5}px, ${r.top - h.top + r.height / 2 - 5}px)`;
    },
    park: chip => chip.classList.remove('is-live'),
    add(cls){
      const el = document.createElement('i');
      el.className = cls;
      host.appendChild(el);
      return el;
    },
  };
}

/** The three states the play button can be in, named the same way everywhere. */
export function paintTransport(els, {playing, done, round, rounds, unit = 'round'}){
  const mode = playing ? 'pause' : (done ? 'replay' : 'play');
  els.toggle.dataset.state = mode;
  const label = {play: 'Play the replay', pause: 'Pause the replay', replay: 'Start over'}[mode];
  els.toggle.setAttribute('aria-label', label);
  els.toggle.title = label;
  els.round.textContent = round < 0 ? `${rounds} ${unit}s` : `${unit} ${round + 1} of ${rounds}`;
  els.prev.disabled = round <= 0;
  els.next.disabled = round >= rounds - 1;
}
