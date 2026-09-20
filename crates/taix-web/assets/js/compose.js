// Typing into a terminal with a thumb.
//
// The whole design follows from one fact: a keystroke sent one at a time
// over a phone network echoes back visibly late, and a terminal that echoes
// late is unusable - you type ahead of what you can see, autocorrect argues
// with the shell, and every mistake costs a round trip to notice. So the
// line is composed *locally*, in a real text field the phone renders at
// native speed, and crosses the network once when it is finished. The pane
// echoes it at the prompt while it is being typed (see `state.staged`), so
// it still reads as typing into the terminal rather than into a form.
//
// What genuinely needs a round trip - Tab completion, ^C, arrows through
// the shell's own history - flushes the staged line first and then sends
// the raw key, because Tab completes nothing while the word it should
// complete is still sitting in the browser.

import {
  state,
  on,
  send,
  mobile,
  driving,
  claim,
  drop,
  shown,
  windowById,
  el,
  h,
  stage,
  toast,
  failed,
  menu,
  bytes,
  drafts,
} from './store.js';

// Lines this browser has sent, newest first. This is the completion a
// phone has before the shell's: prefix-matched, tappable, instant.
const HISTORY_MAX = 60;
const SUGGEST_MAX = 6;
let history = [];
try {
  history = JSON.parse(localStorage.getItem('taix-history') || '[]');
} catch {
  history = [];
}

// Sticky modifiers. They apply to the next key from anywhere, including a
// character from the system keyboard, which is how ^P or M-x is reachable
// at all on a phone.
let heldCtrl = false;
let heldAlt = false;

// Whether the symbol row is up. The characters a shell needs most are the
// ones a phone keyboard buries two layers deep, so it defaults on.
let syms = localStorage.getItem('taix-syms') !== 'off';

// Windows this page decided to only read, so the claim prompt stops
// covering the pane after it has been answered once.
const reading = new Set();

// The last lock, to say out loud when the keyboard changes hands.
let hadLock = null;

const buzz = (ms = 6) => navigator.vibrate?.(ms);

// Keys are in the order of need, not of a keyboard: what a TUI cannot be
// driven without first, then the line-editing controls, then the
// punctuation a phone hides.
const NAV_KEYS = [
  { key: 'Escape', label: 'esc' },
  { key: 'Tab', label: 'tab' },
  { key: 'C-c', label: '^C' },
  { key: 'Up', label: '↑' },
  { key: 'Down', label: '↓' },
  { key: 'Left', label: '←' },
  { key: 'Right', label: '→' },
  { key: 'ctrl', label: 'ctrl' },
  { key: 'sym', label: '#' },
];

const SYM_KEYS = [
  '/',
  '-',
  '_',
  '.',
  '|',
  '~',
  '$',
  '*',
  '"',
  "'",
  '`',
  '=',
  ':',
  '&',
  '(',
  ')',
  '[',
  ']',
  '{',
  '}',
  '<',
  '>',
  '!',
  '?',
  '#',
  '%',
  '\\',
  ';',
  ',',
  '@',
  '+',
];

const CTL_KEYS = [
  { key: 'C-u', label: '^U', title: 'Clear the line' },
  { key: 'C-a', label: '^A', title: 'Start of line' },
  { key: 'C-e', label: '^E', title: 'End of line' },
  { key: 'C-w', label: '^W', title: 'Delete a word' },
  { key: 'C-r', label: '^R', title: 'Search history' },
  { key: 'C-d', label: '^D', title: 'End of input' },
  { key: 'C-z', label: '^Z', title: 'Suspend' },
  { key: 'C-l', label: '^L', title: 'Clear the screen' },
  { key: 'alt', label: 'alt' },
  { key: 'Home', label: 'home' },
  { key: 'End', label: 'end' },
  { key: 'PPage', label: 'pgup' },
  { key: 'NPage', label: 'pgdn' },
  { key: 'BSpace', label: '⌫' },
];

let text = null;
let form = null;
let searching = false;
let lastShown = null;

/** Put the caret where a keystroke belongs on this screen. */
export function focusInput() {
  (mobile() ? text : el('sink')).focus();
}

export function mountCompose() {
  const input = el('input');
  const claimBox = el('claim');
  const prompt = el('prompt');
  const sugg = el('sugg');
  const keys = el('keys');
  form = el('entry');
  text = el('entry-text');

  buildKeys(keys);

  // ---- the line ----

  // What the field held before this edit, so a sticky modifier can pull the
  // character back out and send it as a key instead.
  let previous = '';

  text.addEventListener('input', () => {
    if ((heldCtrl || heldAlt) && text.value.length === previous.length + 1) {
      const at = text.selectionStart - 1;
      const ch = text.value[at];
      text.value = previous;
      text.setSelectionRange(at, at);
      const mods = [heldCtrl ? 'C' : null, heldAlt ? 'M' : null].filter(Boolean).join('-');
      const id = shown();
      if (id && driving(id)) {
        flush(id);
        send({ kind: 'key', window: id, name: `${mods}-${ch}` });
      }
      clearMods();
    }
    previous = text.value;
    stage(text.value);
    const id = shown();
    if (id) drafts.set(id, text.value);
    if (searching && !text.value.trim()) searching = false;
    grow();
    paintSuggestions(sugg);
  });

  // A hardware keyboard on a tablet: Enter sends, Shift+Enter keeps
  // composing. The soft keyboard's own send key arrives as a form submit.
  text.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      submit();
    }
    if (e.key === 'Escape' && searching) {
      e.preventDefault();
      searching = false;
      paintSuggestions(sugg);
    }
  });

  form.addEventListener('submit', (e) => {
    e.preventDefault();
    submit();
  });

  // Taking focus is the same intent as tapping the pane: this is where I am
  // typing. Claiming here rather than through a separate button is what
  // removes a whole bar from the screen.
  text.addEventListener('focus', () => {
    const id = shown();
    if (id && !driving(id)) claim(id);
  });

  // ---- suggestions ----

  // `pointerdown` so the keyboard never drops; `click` with no pointer
  // behind it is a keyboard or a screen reader, which gets the same result.
  const takeSuggestion = (chip) => {
    if (chip.dataset.key === 'C-r') {
      searching = false;
      const id = target();
      if (id) { flush(id); send({ kind: 'key', window: id, name: 'C-r' }); }
      buzz();
      text.focus();
      return;
    }
    searching = false;
    text.value = chip.dataset.line;
    previous = text.value;
    stage(text.value);
    grow();
    paintSuggestions(sugg);
    buzz();
    text.focus();
    text.setSelectionRange(text.value.length, text.value.length);
  };

  sugg.addEventListener('pointerdown', (e) => {
    const chip = e.target.closest('.sug');
    if (!chip) return;
    e.preventDefault();
    takeSuggestion(chip);
  });

  sugg.addEventListener('click', (e) => {
    const chip = e.target.closest('.sug');
    if (chip && e.detail === 0) takeSuggestion(chip);
  });

  // ---- keys ----

  // `pointerdown`, not `click`: a tap that reaches a button's default
  // action moves focus out of the field, which drops the soft keyboard and
  // made iOS commit its pending text as the row's own label.
  keys.addEventListener('pointerdown', (e) => {
    const key = e.target.closest('.key');
    if (!key) return;
    e.preventDefault();
    pressKey(key);
  });
  // Assistive and hardware activation, which sends no pointer event.
  keys.addEventListener('click', (e) => {
    const key = e.target.closest('.key');
    if (key && e.detail === 0) pressKey(key);
  });

  // ---- prompt (answer card for waiting state) ----

  prompt.addEventListener('click', (e) => {
    const btn = e.target.closest('.prompt-btn');
    if (!btn) return;
    const id = shown();
    if (!id) return;
    const mine = driving(id);
    // Borrow claim if needed.
    if (!mine) claim(id);
    flush(id);
    const action = btn.dataset.action;
    if (action === 'y') {
      send({ kind: 'type', window: id, text: 'y' });
      send({ kind: 'key', window: id, name: 'Enter' });
    } else if (action === 'enter') {
      send({ kind: 'key', window: id, name: 'Enter' });
    } else if (action === 'reply') {
      text.focus();
    } else if (action === 'c-c') {
      send({ kind: 'interrupt', window: id });
    }
    if (!mine) drop(id);
  });

  // ---- claiming ----

  el('claim-go').addEventListener('click', () => {
    const id = shown();
    if (!id) return;
    reading.delete(id);
    claim(id);
    text.focus();
  });

  el('claim-read').addEventListener('click', () => {
    const id = shown();
    if (id) reading.add(id);
    render();
  });

  // ---- photos and files ----

  // An agent reads files, so "add an image" means: put the bytes in the
  // project it is running in and name the path. The alternative - a chat
  // pane with attachments - would be a second way to talk to something
  // that already takes a command line.
  el('entry-add').addEventListener('click', () => {
    menu(
      'Add to this line',
      [
        { label: 'Photo or camera', hint: 'image', run: () => el('pick-image').click() },
        { label: 'File', hint: 'any', run: () => el('pick-file').click() },
      ],
      el('entry-add'),
    );
  });

  for (const id of ['pick-image', 'pick-file']) {
    el(id).addEventListener('change', (e) => {
      const files = [...e.target.files];
      // Cleared so picking the same photo twice still fires `change`.
      e.target.value = '';
      addFiles(files);
    });
  }

  /** Upload each file, then leave its path in the line, ready to send. */
  async function addFiles(files) {
    for (const file of files) {
      toast(`Sending ${file.name} · ${bytes(file.size / 1024)}`);
      try {
        const res = await fetch(`/api/upload?name=${encodeURIComponent(file.name)}`, {
          method: 'POST',
          body: file,
        });
        const body = await res.json();
        if (!res.ok) throw new Error(body.error || `upload failed (${res.status})`);
        insert(shellWord(body.path));
        toast(`Added ${body.name}`);
        buzz(10);
      } catch (e) {
        failed(e);
      }
    }
  }

  on(render);

  /**
   * The window this is typing into, claimed if it was not already.
   *
   * Claiming on the first thing you type, rather than gating the field
   * behind a button, is what makes the phone feel like a terminal: the
   * commands queue in order on one connection, so the takeover is drained
   * on the same frame as the keys that follow it and nothing is lost.
   */
  function target() {
    const id = shown();
    if (!id) return null;
    claim(id);
    return id;
  }

  function submit() {
    const id = target();
    if (!id) return;
    const line = text.value;
    flush(id);
    send({ kind: 'key', window: id, name: 'Enter' });
    remember(line);
    paintSuggestions(sugg);
    buzz(10);
    // Focus never left, so the keyboard stays up; this only matters when
    // the send button was tapped from a page that had lost it.
    text.focus();
  }

  /**
   * Hand what is staged to the shell's own line editor without submitting
   * it. Every raw key does this first, so Tab has a word to complete and
   * ^C has a command to interrupt.
   */
  function flush(id) {
    const line = text.value;
    if (!line) return false;
    text.value = '';
    previous = '';
    stage('');
    if (id) drafts.set(id, '');
    grow();
    send(line.includes('\n') ? { kind: 'paste', window: id, text: line } : { kind: 'type', window: id, text: line });
    return true;
  }

  function pressKey(key) {
    const code = key.dataset.key;
    if (code === 'sym') {
      syms = !syms;
      localStorage.setItem('taix-syms', syms ? 'on' : 'off');
      paintSyms(keys);
      buzz();
      return;
    }
    if (code === 'ctrl' || code === 'alt') {
      const held = code === 'ctrl' ? (heldCtrl = !heldCtrl) : (heldAlt = !heldAlt);
      key.classList.toggle('on', held);
      // `data-keys-ctl` belongs to the settings toggle, not to a held
      // modifier: the button's own `.on` is what shows a sticky Ctrl.
      buzz();
      return;
    }
    const id = target();
    if (!id) return;
    buzz();

    // A character belongs in the line being composed - these are the keys
    // a phone keyboard hides, which is the whole reason the row exists. No
    // network, no echo wait, and the caret stays where it was.
    if (code.length === 1 && !heldCtrl && !heldAlt) {
      text.setRangeText(code, text.selectionStart, text.selectionEnd, 'end');
      previous = text.value;
      stage(text.value);
      grow();
      paintSuggestions(sugg);
      text.focus();
      return;
    }
    if (code === 'Enter' && !heldCtrl && !heldAlt) {
      submit();
      return;
    }
    if (code === 'C-r') {
      searching = !searching;
      paintSuggestions(sugg);
      buzz();
      text.focus();
      return;
    }
    flush(id);
    send({ kind: 'key', window: id, name: withMods(code) });
    clearMods();
    text.focus();
  }

  /**
   * Put text in the line where the caret is, exactly as a key row tap
   * does: locally, with the staged echo and the field's height following.
   */
  function insert(word) {
    const pad = text.value && !/\s$/.test(text.value.slice(0, text.selectionStart)) ? ' ' : '';
    text.setRangeText(`${pad}${word} `, text.selectionStart, text.selectionEnd, 'end');
    previous = text.value;
    stage(text.value);
    grow();
    paintSuggestions(sugg);
    text.focus();
  }

  function render() {
    if (!mobile() || state.view !== 'stage') {
      input.hidden = true;
      return;
    }
    const id = shown();
    const win = windowById(id);
    input.hidden = !win;
    if (!win) return;

    if (id !== lastShown) {
      if (lastShown) drafts.set(lastShown, text.value);
      text.value = drafts.get(id);
      previous = text.value;
      stage(text.value);
      grow();
      lastShown = id;
    }

    const mine = driving(id);
    announce();

    // The field is always there, claimed or not: typing into it is the
    // claim. The prompt above it is the explicit offer, for the first time
    // and for anyone who would rather just read.
    claimBox.hidden = mine || reading.has(id);
    sugg.hidden = !sugg.childElementCount;
    text.placeholder = mine ? `Type into ${win.name}` : `Type to take ${win.name}`;
    paintSuggestions(sugg);

    // Answer card for waiting state.
    if (win.state === 'waiting' && !claimBox.hidden === false) {
      prompt.hidden = false;
      prompt.replaceChildren(
        h('div', { class: 'prompt-q' }, `${win.name} is waiting for an answer`),
        h('div', { class: 'prompt-row' },
          h('button', { class: 'prompt-btn accent', dataset: { action: 'y' } }, 'Yes'),
          h('button', { class: 'prompt-btn', dataset: { action: 'enter' } }, 'Enter'),
          h('button', { class: 'prompt-btn', dataset: { action: 'reply' } }, 'Reply'),
          h('button', { class: 'prompt-btn danger', dataset: { action: 'c-c' } }, '^C'),
        ),
      );
    } else {
      prompt.hidden = true;
    }
  }

  function grow() {
    // Up to five lines, then it scrolls: a pasted block must not push the
    // terminal off the screen.
    text.style.height = 'auto';
    const line = parseFloat(getComputedStyle(text).lineHeight) || 21;
    text.style.height = `${Math.min(text.scrollHeight, line * 5 + 16)}px`;
  }
}

/**
 * Say when the keyboard changes hands.
 *
 * Losing a terminal mid-sentence - because someone clicked that pane on the
 * desktop - is the one thing that happens to this page without it asking,
 * so it is the one thing it has to be told.
 */
function announce() {
  const now = state.snap?.lock?.remote || [];
  if (hadLock === null) {
    hadLock = now.slice();
    return;
  }
  const name = (id) => windowById(id)?.name || `window ${id}`;
  for (const id of now) {
    if (!hadLock.includes(id)) toast(`Keyboard is yours — ${name(id)}`);
  }
  for (const id of hadLock) {
    if (!now.includes(id)) {
      toast(`The desktop took ${name(id)} back`, 'warn');
      buzz(30);
    }
  }
  hadLock = now.slice();
}

/**
 * A path as one word for a shell. The server already reduced the name to
 * characters a command line takes verbatim, so this is the belt for a
 * project directory with a space in it.
 */
function shellWord(path) {
  if (/^[\w@%+=:,./-]+$/.test(path)) return path;
  return `'${path.replaceAll("'", `'\\''`)}'`;
}

function withMods(code) {
  const mods = [heldCtrl ? 'C' : null, heldAlt ? 'M' : null].filter(Boolean);
  if (mods.length === 0 || code.includes('-')) return code;
  return `${mods.join('-')}-${code}`;
}

function clearMods() {
  heldCtrl = false;
  heldAlt = false;
  for (const key of document.querySelectorAll('#keys [data-key="ctrl"], #keys [data-key="alt"]')) {
    key.classList.remove('on');
  }
}

function buildKeys(keys) {
  const key = ({ key, label, title }, group = '') =>
    h('button', { class: `key ${group}`.trim(), type: 'button', 'data-key': key, title: title || null }, label);
  keys.replaceChildren(
    h('div', { class: 'krow' }, ...NAV_KEYS.map((k) => key(k))),
    h(
      'div',
      { class: 'krow scroll', id: 'krow-sym' },
      // Punctuation first: a command line needs `/`, `-` and `.` far more
      // often than it needs ^Z, and the row scrolls.
      ...SYM_KEYS.map((ch) => key({ key: ch, label: ch }, 'sym')),
      ...CTL_KEYS.map((k) => key(k, 'ctl')),
    ),
  );
  paintSyms(keys);
}

function paintSyms(keys) {
  el('krow-sym').hidden = !syms;
  keys.querySelector('[data-key="sym"]').classList.toggle('on', syms);
}

function remember(line) {
  const command = line.trim();
  if (!command) return;
  history = [command, ...history.filter((other) => other !== command)].slice(0, HISTORY_MAX);
  localStorage.setItem('taix-history', JSON.stringify(history));
}

/**
 * The strip above the field: what this phone ran before, prefix-matched.
 *
 * A tap fills the line rather than running it - a command you half
 * remember is one you want to look at before it goes to a shell - and it
 * costs nothing, because the match is against this page's own history.
 */
function paintSuggestions(sugg) {
  const typed = text.value.trim();
  if (searching) {
    const matches = history.filter((line) => line.toLowerCase().includes(typed.toLowerCase())).slice(0, 8);
    const chips = matches.map((line) => h('button', { class: 'sug mono', type: 'button', dataset: { line } }, line));
    chips.push(h('button', { class: 'sug', type: 'button', dataset: { key: 'C-r' } }, '^R shell'));
    sugg.replaceChildren(...chips);
    sugg.hidden = false;
    return;
  }
  // Only against something typed: an empty field showing the last six
  // commands cost a permanent row of the screen for a list nobody read.
  const matches = typed
    ? history.filter((line) => line !== typed && line.toLowerCase().startsWith(typed.toLowerCase())).slice(0, SUGGEST_MAX)
    : [];
  if (matches.length === 0) {
    sugg.hidden = true;
    sugg.replaceChildren();
    return;
  }
  sugg.replaceChildren(
    ...matches.map((line) => h('button', { class: 'sug mono', type: 'button', dataset: { line } }, line)),
  );
  sugg.hidden = false;
}
