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
  copy,
  sym,
  api,
  project,
  liveMarks,
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

// The last lock, to say out loud when the keyboard changes hands.
let hadLock = null;

// Live typing on a phone: every key goes straight to the pane, as in a
// real terminal, so completion menus and TUIs react to each character.
// Off, the line is staged here and sent whole. A tablet is always live.
let live = localStorage.getItem('taix-live') === '1';
export const liveTyping = () => live;

// iOS has no `navigator.vibrate`, but 18+ plays the system haptic when a
// `switch` checkbox is toggled by a click inside a user gesture. It is
// `display: none` so nothing can focus it; focus is restored anyway,
// because a caret that moved would drop the soft keyboard.
let tick = null;
export function buzz(ms = 6) {
  if (navigator.vibrate) return navigator.vibrate(ms);
  if (!('switch' in HTMLInputElement.prototype) && !/iP(hone|ad)/.test(navigator.userAgent)) return;
  if (!tick) {
    tick = h('label', { 'aria-hidden': 'true', style: 'display:none' }, h('input', { type: 'checkbox', switch: true, tabindex: '-1' }));
    document.body.append(tick);
  }
  const was = document.activeElement;
  tick.click();
  if (document.activeElement !== was) was?.focus();
}

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

// Keys this phone added itself: `{ label, kind, value }`, where `kind` is
// `type` (into the line), `run` (into the line, then Enter) or `key` (a
// named key from the lists above). They sit after the nav keys, because a
// key someone bothered to make is one they want under the thumb.
const MINE = 'taix-keys-mine';
export function myKeys() {
  try {
    const list = JSON.parse(localStorage.getItem(MINE));
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

/** Store the list and tell the key row, and the settings panel, it changed. */
export function saveMyKeys(list) {
  if (list.length) localStorage.setItem(MINE, JSON.stringify(list));
  else localStorage.removeItem(MINE);
  window.dispatchEvent(new Event(MINE));
}

// What a custom key can press: every named key in the row, minus the
// sticky modifiers, which mean nothing as a key of their own.
const NAMED = [...NAV_KEYS, { key: 'Enter', label: '⏎' }, ...CTL_KEYS].filter((k) => k.key !== 'ctrl' && k.key !== 'alt');
let mineKeys = [];

let text = null;
let form = null;
let searching = false;
let lastShown = null;
// Files attached to each window's line, shown as chips over the field and
// sent as `[n] path` in front of it: `{ n, path, name, size, image }`.
// Kept beside the drafts, so a reload still knows what the draft's `[n]`
// marks refer to; image previews come from the server (`/api/raw`).
const attached = loadMap('taix-attached');
// What each window was last sent, newest first: the faded "sent" chips,
// one tap from being attached again.
const sent = loadMap('taix-sent');
// The files a remembered line carried, by line, so recalling it from the
// history strip brings its chips back with it.
let lineFiles = {};
try {
  lineFiles = JSON.parse(localStorage.getItem('taix-line-files') || '{}');
} catch {}
// An `@name` being typed: where it sits in the line and what it matches.
let found = null;
let paintFiles = () => {};
// Set by mountCompose: the one way a file reaches the line.
let attachHere = () => {};

/** Put the caret where a keystroke belongs on this screen. */
export function focusInput() {
  (mobile() && !live ? text : el('sink')).focus();
}

export function mountCompose() {
  const input = el('input');
  const prompt = el('prompt');
  const sugg = el('sugg');
  const keys = el('keys');
  form = el('entry');
  text = el('entry-text');
  const ink = el('entry-ink');
  text.addEventListener('scroll', () => { ink.scrollTop = text.scrollTop; });
  const filesEl = el('entry-files');

  buildKeys(keys);
  window.addEventListener(MINE, () => buildKeys(keys));

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
    prune();
    mention();
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
    if (chip.dataset.pick) return pickMention(chip.dataset.pick);
    text.value = chip.dataset.line;
    // A remembered line brings back the files its marks refer to.
    const id = shown();
    const files = lineFiles[chip.dataset.line];
    if (id && files) {
      attached.set(id, files.map((f) => ({ ...f })));
      saveMap('taix-attached', attached);
      paintFiles();
    }
    changed();
    prune();
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

  // `pointerdown` is cancelled, not `click`: a tap that reaches a button's
  // default action moves focus out of the field, which drops the soft
  // keyboard and made iOS commit its pending text as the row's own label.
  // The key itself fires on release, and only if the finger did not turn
  // into a swipe along the row - the browser cancels the pointer then.
  let pressed = null;
  let held = 0;
  // A long press is a different gesture from a tap, so the release that
  // follows it must not also fire the key: `pressed` is cleared as the
  // sheet opens, and `pointerup` only fires what is still pressed.
  const longPress = () => {
    const key = pressed;
    pressed = null;
    if (!key) return;
    buzz(12);
    const at = Number(key.dataset.mine ?? -1);
    const mine = mineKeys[at];
    if (!mine) return editKey(-1);
    const move = (by) => {
      const list = myKeys();
      [list[at], list[at + by]] = [list[at + by], list[at]];
      saveMyKeys(list);
    };
    menu({ title: mine.label, subtitle: mine.value }, [
      { label: 'Edit', run: () => editKey(at) },
      at > 0 && { label: 'Move left', run: () => move(-1) },
      at < mineKeys.length - 1 && { label: 'Move right', run: () => move(1) },
      '-',
      { label: 'Delete', danger: true, run: () => saveMyKeys(myKeys().filter((_, i) => i !== at)) },
    ]);
  };
  keys.addEventListener('pointerdown', (e) => {
    pressed = e.target.closest('.key');
    if (!pressed) return;
    e.preventDefault();
    if (!pressed.classList.contains('mode')) held = setTimeout(longPress, 450);
  });
  keys.addEventListener('pointerup', (e) => {
    clearTimeout(held);
    const key = e.target.closest('.key');
    if (key && key === pressed) pressKey(key);
    pressed = null;
  });
  keys.addEventListener('pointercancel', () => {
    clearTimeout(held);
    pressed = null;
  });
  // iOS offers its own long-press menu on a held button.
  keys.addEventListener('contextmenu', (e) => e.preventDefault());
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
    if (btn.dataset.send) {
      send({ kind: 'type', window: id, text: btn.dataset.send });
      send({ kind: 'key', window: id, name: 'Enter' });
    } else if (action === 'enter') {
      send({ kind: 'key', window: id, name: 'Enter' });
    } else if (action === 'reply') {
      focusInput();
    } else if (action === 'c-c') {
      send({ kind: 'interrupt', window: id });
    }
    if (!mine) drop(id);
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
        { label: 'Recent attachments', hint: '14 days', run: tray },
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

  // A screenshot pasted into the field is an attachment, as on the desktop.
  text.addEventListener('paste', (e) => {
    const files = [...(e.clipboardData?.files || [])];
    if (!files.length) return;
    e.preventDefault();
    addFiles(files);
  });

  // Tapping a `[n]` card in the field opens its file, like its chip. The
  // caret lands *inside* the three characters only when the tap was on the
  // card; a tap beside it, to type there, leaves the caret at its edge.
  text.addEventListener('click', () => {
    const at = text.selectionStart;
    if (at !== text.selectionEnd) return;
    for (const m of text.value.matchAll(/\[\d+\]/g)) {
      if (m.index < at && at < m.index + m[0].length) {
        const f = (attached.get(shown()) || []).find((x) => mark(x.n) === m[0]);
        if (f) openFile(f, text);
        return;
      }
    }
  });

  /**
   * Attach a file already on the server's disk: a chip, and its `[n]` at
   * the caret, so the prose can point at it ("the error is [1]"). Live
   * typing has no line to hold it, so there the path goes straight in.
   */
  attachHere = (file) => {
    const id = target();
    if (!id) return;
    const list = attached.get(id) || [];
    const n = Math.max(0, ...list.map((f) => f.n)) + 1;
    // Live typing, or a tablet's raw keys, has no line to hold a chip.
    if (live || !mobile()) {
      const typed = (liveMarks.get(id) || 0) + 1;
      liveMarks.set(id, typed);
      send({ kind: 'type', window: id, text: `${mark(typed)} ${shellWord(file.path)} ` });
      return;
    }
    attached.set(id, [...list, { n, path: file.path, name: file.name, size: file.size || 0, image: isImage(file.name) }]);
    saveMap('taix-attached', attached);
    // A mark glued to the word before it would read as part of that word.
    const glued = text.selectionStart > 0 && !/\s/.test(text.value[text.selectionStart - 1]);
    text.setRangeText(`${glued ? ' ' : ''}${mark(n)} `, text.selectionStart, text.selectionEnd, 'end');
    changed();
    paintFiles();
    text.focus();
  };

  /** Everything that follows an edit the `input` event does not see. */
  function changed() {
    previous = text.value;
    stage(text.value);
    const id = shown();
    if (id) drafts.set(id, text.value);
    grow();
    paintSuggestions(sugg);
  }

  /** Deleting a file's mark from the line deletes the file. */
  function prune() {
    const id = shown();
    const files = attached.get(id) || [];
    const kept = files.filter((f) => text.value.includes(mark(f.n)));
    if (kept.length === files.length) return;
    attached.set(id, kept);
    saveMap('taix-attached', attached);
    paintFiles();
    grow();
  }

  function removeFile(id, f) {
    text.value = text.value.replaceAll(`${mark(f.n)} `, '').replaceAll(mark(f.n), '');
    changed();
    prune();
  }

  /** `@part` before the caret: the project's files that match, in the strip. */
  let findTimer = 0;
  function mention() {
    clearTimeout(findTimer);
    const before = text.value.slice(0, text.selectionStart);
    const m = before.match(/(?:^|\s)@([^\s@]*)$/);
    const root = m && project(windowById(shown())?.project)?.path;
    if (!root) {
      if (found) {
        found = null;
        paintSuggestions(sugg);
      }
      return;
    }
    findTimer = setTimeout(async () => {
      try {
        const data = await api(`/api/find?path=${encodeURIComponent(root)}&q=${encodeURIComponent(m[1])}`);
        found = { start: before.length - m[1].length - 1, end: before.length, entries: data.entries.slice(0, 8) };
        paintSuggestions(sugg);
      } catch {}
    }, 120);
  }

  /** The picked file replaces the `@part` that found it. */
  function pickMention(path) {
    if (found && text.value[found.start] === '@') {
      text.setRangeText('', found.start, found.end, 'end');
    }
    found = null;
    attachHere({ path, name: path.split('/').pop() });
    buzz();
  }

  /** A chip's sheet: preview, insert its mark again, draw on it, remove. */
  function openFile(f, anchor) {
    const id = shown();
    menu(
      {
        title: f.name,
        subtitle: f.size ? bytes(f.size / 1024) : null,
        preview: f.image ? h('img', { class: 'sheet-preview', src: raw(f.path), alt: f.name }) : null,
      },
      [
        {
          label: `Insert ${mark(f.n)} at the cursor`,
          run: () => {
            text.setRangeText(`${mark(f.n)} `, text.selectionStart, text.selectionEnd, 'end');
            changed();
            text.focus();
          },
        },
        f.image && { label: 'Draw on it', run: () => markup(f) },
        { label: 'Copy path', note: f.path, hint: sym('copy'), run: () => copy(f.path, 'Copied path') },
        '-',
        { label: 'Remove', danger: true, run: () => removeFile(id, f) },
      ],
      anchor,
    );
  }

  /**
   * A pen and a box over the image, saved as a new file in its chip's
   * place: an arrow at the broken button says more than a paragraph.
   */
  function markup(f) {
    const img = new Image();
    img.onerror = () => toast('Could not load the image', 'warn');
    img.onload = () => {
      const canvas = h('canvas', { class: 'markup', width: img.naturalWidth, height: img.naturalHeight });
      const ctx = canvas.getContext('2d');
      const strokes = [];
      let tool = 'pen';
      let cur = null;
      const paint = () => {
        ctx.drawImage(img, 0, 0);
        ctx.strokeStyle = '#ff3b30';
        ctx.lineWidth = Math.max(3, img.naturalWidth / 160);
        ctx.lineCap = 'round';
        ctx.lineJoin = 'round';
        for (const s of cur ? [...strokes, cur] : strokes) {
          const [a, b] = [s.pts[0], s.pts[s.pts.length - 1]];
          ctx.beginPath();
          if (s.box) ctx.rect(a.x, a.y, b.x - a.x, b.y - a.y);
          else s.pts.forEach((p, i) => (i ? ctx.lineTo(p.x, p.y) : ctx.moveTo(p.x, p.y)));
          ctx.stroke();
        }
      };
      const at = (e) => {
        const r = canvas.getBoundingClientRect();
        return { x: ((e.clientX - r.left) * canvas.width) / r.width, y: ((e.clientY - r.top) * canvas.height) / r.height };
      };
      canvas.addEventListener('pointerdown', (e) => {
        canvas.setPointerCapture(e.pointerId);
        cur = { box: tool === 'box', pts: [at(e)] };
      });
      canvas.addEventListener('pointermove', (e) => {
        if (!cur) return;
        cur.pts.push(at(e));
        paint();
      });
      canvas.addEventListener('pointerup', () => {
        if (cur) strokes.push(cur);
        cur = null;
        paint();
      });
      const pick = (name) => () => {
        tool = name;
        pen.classList.toggle('accent', name === 'pen');
        box.classList.toggle('accent', name === 'box');
      };
      const pen = h('button', { class: 'accent', text: 'Pen', onclick: pick('pen') });
      const box = h('button', { text: 'Box', onclick: pick('box') });
      const done = () => {
        el('modal').hidden = true;
      };
      const save = () => canvas.toBlob(async (blob) => {
        try {
          const body = await upload(new File([blob], `${f.name.replace(/\.[^.]+$/, '')}-marked.png`, { type: 'image/png' }));
          Object.assign(f, { path: body.path, name: body.name, size: blob.size });
          saveMap('taix-attached', attached);
          paintFiles();
          done();
        } catch (e) {
          failed(e);
        }
      }, 'image/png');
      el('modal-card').replaceChildren(
        h('header', { class: 'mono', text: `Draw on ${mark(f.n)}` }),
        canvas,
        h(
          'footer',
          {},
          pen,
          box,
          h('button', { text: 'Undo', onclick: () => { strokes.pop(); paint(); } }),
          h('button', { text: 'Cancel', onclick: done }),
          h('button', { class: 'accent', text: 'Save', onclick: save }),
        ),
      );
      paint();
      el('modal').hidden = false;
    };
    img.src = raw(f.path);
  }

  // Dictation: spoken text lands at the caret, and "image two" or "file
  // one" becomes that file's `[n]` when such a chip exists.
  const Rec = window.SpeechRecognition || window.webkitSpeechRecognition;
  const mic = el('entry-mic');
  let rec = null;
  mic.hidden = !Rec;
  mic.addEventListener('pointerdown', (e) => e.preventDefault());
  mic.addEventListener('click', () => {
    if (rec) return rec.stop();
    rec = new Rec();
    rec.lang = navigator.language;
    rec.continuous = true;
    rec.onresult = (e) => {
      for (let i = e.resultIndex; i < e.results.length; i++) {
        if (!e.results[i].isFinal) continue;
        text.setRangeText(`${spoken(e.results[i][0].transcript.trim())} `, text.selectionStart, text.selectionEnd, 'end');
        changed();
      }
    };
    rec.onerror = (e) => toast(`Dictation: ${e.error}`, 'warn');
    rec.onend = () => {
      rec = null;
      mic.classList.remove('on');
    };
    rec.start();
    mic.classList.add('on');
    buzz(10);
  });

  paintFiles = () => {
    const id = shown();
    const list = attached.get(id) || [];
    const past = (sent.get(id) || []).filter((s) => !list.some((f) => f.path === s.path));
    filesEl.hidden = !list.length && !past.length;
    const face = (f, cls, style) => {
      const dot = f.name.lastIndexOf('.');
      const chip = h(
        'button',
        { type: 'button', class: cls, title: f.path, style },
        sym(f.image ? 'image' : 'file'),
        f.n ? h('span', { class: 'num', text: String(f.n) }) : null,
        h('span', { class: 'stem', text: dot > 0 ? f.name.slice(0, dot) : f.name }),
        dot > 0 ? h('span', { class: 'ext', text: f.name.slice(dot) }) : null,
      );
      // `pointerdown`, like the key row: a tap that moved focus would drop
      // the soft keyboard.
      chip.addEventListener('pointerdown', (e) => e.preventDefault());
      return chip;
    };
    filesEl.replaceChildren(
      ...list.map((f) => {
        const chip = face(f, 'file-chip', `color:${tint(f.n)};border-color:${tint(f.n, 0.45)};background:${tint(f.n, 0.12)}`);
        // Dropped into the field, a chip is its mark, wherever it lands.
        chip.draggable = true;
        chip.addEventListener('dragstart', (e) => e.dataTransfer.setData('text/plain', `${mark(f.n)} `));
        chip.onclick = () => openFile(f, chip);
        return chip;
      }),
      ...(past.length ? [h('span', { class: 'sent-label', text: 'sent' })] : []),
      ...past.map((f) => {
        const chip = face({ ...f, n: 0 }, 'file-chip sent');
        chip.onclick = () => menu(
          {
            title: f.name,
            subtitle: 'Sent from this line',
            preview: f.image ? h('img', { class: 'sheet-preview', src: raw(f.path), alt: f.name }) : null,
          },
          [
            { label: 'Attach again', run: () => attachHere(f) },
            { label: 'Copy path', note: f.path, hint: sym('copy'), run: () => copy(f.path, 'Copied path') },
            '-',
            {
              label: 'Clear sent files',
              danger: true,
              run: () => {
                sent.delete(id);
                saveMap('taix-sent', sent);
                paintFiles();
              },
            },
          ],
          chip,
        );
        return chip;
      }),
    );
  };

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
    const files = attached.get(id) || [];
    flush(id);
    send({ kind: 'key', window: id, name: 'Enter' });
    remember(line, files);
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
    const files = attached.get(id) || [];
    const line = [...files.map((f) => `${mark(f.n)} ${shellWord(f.path)}`), text.value].filter(Boolean).join(' ');
    if (!line) return false;
    text.value = '';
    previous = '';
    stage('');
    if (id) drafts.set(id, '');
    if (files.length) {
      const old = (sent.get(id) || []).filter((s) => !files.some((f) => f.path === s.path));
      sent.set(id, [...files, ...old].slice(0, 6));
      saveMap('taix-sent', sent);
    }
    attached.delete(id);
    saveMap('taix-attached', attached);
    paintFiles();
    grow();
    send(line.includes('\n') ? { kind: 'paste', window: id, text: line } : { kind: 'type', window: id, text: line });
    return true;
  }

  function pressKey(key) {
    if (key.classList.contains('add')) return editKey(-1);
    if (key.classList.contains('mode')) return toggleLive(key);
    const code = key.dataset.key;
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

    // A tablet, or a phone in live mode, types straight into the pane
    // through `#sink`: the row is only the keys its soft keyboard lacks,
    // sent as keys, and focus never leaves the sink (the `pointerdown`
    // above was cancelled).
    if (!mobile() || live) {
      const own = mineKeys[key.dataset.mine];
      if (own && own.kind !== 'key') {
        send({ kind: 'type', window: id, text: own.value });
        if (own.kind === 'run') send({ kind: 'key', window: id, name: 'Enter' });
      } else if (code.length === 1 && !heldCtrl && !heldAlt) {
        send({ kind: 'keys', window: id, text: code });
      } else {
        send({ kind: 'key', window: id, name: withMods(code) });
      }
      clearMods();
      return;
    }

    // A custom key that types is a character key with more characters: it
    // lands in the line, and `run` then sends the line like the send key.
    const mine = mineKeys[key.dataset.mine];
    if (mine && mine.kind !== 'key') {
      text.setRangeText(mine.value, text.selectionStart, text.selectionEnd, 'end');
      previous = text.value;
      stage(text.value);
      grow();
      paintSuggestions(sugg);
      if (mine.kind === 'run') submit();
      else text.focus();
      return;
    }

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
   * Switch the phone between staged lines and live keys. Going live hands
   * the staged line to the shell first, so nothing typed is lost and its
   * completion picks up where the field left off.
   */
  function toggleLive(btn) {
    live = !live;
    localStorage.setItem('taix-live', live ? '1' : '0');
    paintMode(btn);
    const id = shown();
    if (live) {
      if (id) {
        claim(id);
        flush(id);
      }
      toast('Live typing — every key goes straight to the terminal');
    } else {
      if (id) {
        text.value = previous = drafts.get(id);
        stage(text.value);
        grow();
      }
      toast('Buffered — type a line, then send it whole');
    }
    buzz(12);
    render();
    focusInput();
  }

  function render() {
    // A tablet: the key row alone, over the soft keyboard while the sink
    // holds focus. The stylesheet does the showing (`#input.raw`), since
    // the keyboard comes and goes without a snapshot to say so.
    const raw = !mobile();
    input.classList.toggle('raw', raw);
    if (raw) {
      input.hidden = !driving(shown());
      prompt.hidden = true;
      // Rotated out of the phone layout: park the line, or its ghost
      // would keep echoing in a pane that is now typed into directly.
      if (lastShown) {
        drafts.set(lastShown, text.value);
        text.value = previous = '';
        grow();
        stage('');
        lastShown = null;
      }
      return;
    }
    if (state.view !== 'stage') {
      input.hidden = true;
      // Coming back to the stage is an arrival again.
      if (lastShown) drafts.set(lastShown, text.value);
      lastShown = null;
      return;
    }
    const id = shown();
    const win = windowById(id);
    input.hidden = !win;
    if (!win) return;

    if (id !== lastShown) {
      // Live keys leave nothing staged, so there is no draft to swap.
      if (!live) {
        if (lastShown) drafts.set(lastShown, text.value);
        text.value = drafts.get(id);
        previous = text.value;
        stage(text.value);
        grow();
      }
      lastShown = id;
      paintFiles();
      // Opening a terminal on the phone is the claim: the grid becomes the
      // phone's, so the pane fills the screen instead of showing the
      // desktop's shape in a corner of it. Only on arrival - a desktop that
      // takes it back keeps it until this page types or leaves and returns.
      if (win.alive && !driving(id)) claim(id);
    }

    input.classList.toggle('live', live);
    const mine = driving(id);
    announce();

    sugg.hidden = !sugg.childElementCount;
    text.placeholder = mine ? `Type into ${win.name}` : `Type to take ${win.name}`;
    paintSuggestions(sugg);

    // Answer card for waiting state.
    if (win.state === 'waiting') {
      prompt.hidden = false;
      prompt.replaceChildren(
        h('div', { class: 'prompt-q' }, `${win.name} is waiting for an answer`),
        h('div', { class: 'prompt-row' },
          ...['y', 'n', '1', '2', 'continue'].map((t) =>
            h('button', { class: `prompt-btn${t === 'y' ? ' accent' : ''}`, dataset: { send: t } }, t)),
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
    // Each file's `[n]` drawn as a card in its colour (see #entry-ink). The
    // token's own text stays, invisible, so the card is exactly as wide as
    // what the caret steps over in the textarea.
    const files = attached.get(shown()) || [];
    ink.replaceChildren(...text.value.split(/(\[\d+\])/).map((part, i) => {
      const f = i % 2 && files.find((x) => mark(x.n) === part);
      return f
        ? h('span', { class: 'ref', style: `--tint:${tint(f.n)}` }, part,
          h('span', { class: 'card' }, sym(f.image ? 'image' : 'file'), String(f.n)))
        : part;
    }), '\n');
    // Up to five lines, then it scrolls: a pasted block must not push the
    // terminal off the screen.
    text.style.height = 'auto';
    const line = parseFloat(getComputedStyle(text).lineHeight) || 21;
    text.style.height = `${Math.min(text.scrollHeight, line * 5 + 16)}px`;
    ink.scrollTop = text.scrollTop;
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

/** Shown in the line as a picture, previewed through `/api/raw`. */
function isImage(name) {
  return /\.(png|jpe?g|gif|webp|bmp)$/i.test(name);
}

function raw(path) {
  return `/api/raw?path=${encodeURIComponent(path)}`;
}

function loadMap(key) {
  try {
    return new Map(Object.entries(JSON.parse(localStorage.getItem(key) || '{}')).map(([k, v]) => [Number(k), v]));
  } catch {
    return new Map();
  }
}

function saveMap(key, map) {
  localStorage.setItem(key, JSON.stringify(Object.fromEntries([...map].filter(([, v]) => v.length))));
}

/** Put the bytes in the server's stash: `{ path, name }` comes back. */
async function upload(file) {
  const res = await fetch(`/api/upload?name=${encodeURIComponent(file.name)}`, { method: 'POST', body: file });
  const body = await res.json();
  if (!res.ok) throw new Error(body.error || `upload failed (${res.status})`);
  return body;
}

/** Upload each file, then attach it to the line. */
async function addFiles(files) {
  for (const file of files) {
    toast(`Sending ${file.name} · ${bytes(file.size / 1024)}`);
    try {
      const body = await upload(file);
      attachHere({ path: body.path, name: body.name, size: file.size });
      toast(`Added ${body.name}`);
      buzz(10);
    } catch (e) {
      failed(e);
    }
  }
}

/** Attach a file on the server's disk (a project file, a stashed one). */
export function attachFile(file) {
  attachHere(file);
}

/**
 * Attach text - a pane selection, lines of a file - as a file of its own,
 * named after its first line. An agent reads a path; a multi-line paste
 * into its prompt would be sent line by line.
 */
export function attachSnippet(body) {
  const first = body.split('\n').find((line) => line.trim()) || '';
  const stem = first.trim().replace(/[^\w-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 32) || 'snippet';
  return addFiles([new File([body], `${stem}.txt`, { type: 'text/plain' })]);
}

/** The stash, newest first: anything sent from here or the desktop lately. */
async function tray() {
  try {
    const { entries } = await api('/api/stash');
    if (!entries.length) return toast('No attachments in the last 14 days');
    menu(
      { title: 'Recent attachments', subtitle: 'Kept for 14 days' },
      entries.slice(0, 20).map((e) => ({
        label: e.name,
        note: `${ago(e.mtime)} · ${bytes(e.size / 1024)}`,
        hint: isImage(e.name) ? h('img', { class: 'tray-thumb', src: raw(e.path), alt: '', loading: 'lazy' }) : sym('file'),
        run: () => attachHere(e),
      })),
      el('entry-add'),
    );
  } catch (e) {
    failed(e);
  }
}

function ago(secs) {
  const s = Date.now() / 1000 - secs;
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

const NUMBERS = { one: 1, two: 2, three: 3, four: 4, five: 5, six: 6, seven: 7, eight: 8, nine: 9, ten: 10 };

/** Dictated text with "image two" / "file 1" turned into an attached file's `[n]`. */
function spoken(said) {
  const have = attached.get(shown()) || [];
  return said.replace(
    /\b(?:attachment|image|file|picture|photo|screenshot|snippet|number)\s+(\d+|one|two|three|four|five|six|seven|eight|nine|ten)\b/gi,
    (all, w) => {
      const n = NUMBERS[w.toLowerCase()] ?? Number(w);
      return have.some((f) => f.n === n) ? mark(n) : all;
    },
  );
}

/** `[n]`: the reference a file leaves in the line, for the prose to cite. */
function mark(n) {
  return `[${n}]`;
}

/** File n's own colour: golden-angle hues, so no two files share one. */
function tint(n, alpha = 1) {
  return `hsl(${(n * 137.508) % 360} 70% 68% / ${alpha})`;
}

/**
 * The sticky modifiers as a tmux prefix (`C-`, `M-`, `C-M-`), cleared. A
 * tablet's Ctrl in the key row applies to the next character its own
 * keyboard types into `#sink`, or ^C would be out of reach.
 */
export function takeMods() {
  const prefix = withMods('');
  clearMods();
  return prefix;
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

// One row that scrolls sideways: what a TUI cannot be driven without
// first, then punctuation, then the line-editing controls. A second row
// cost a line of terminal on a screen that has few to spare.
function buildKeys(keys) {
  const key = ({ key, label, title }, group = '', extra = {}) =>
    h('button', { class: `key ${group}`.trim(), type: 'button', 'data-key': key, title: title || null, ...extra }, label);
  mineKeys = myKeys();
  keys.replaceChildren(
    paintMode(h('button', { class: 'key mode', type: 'button' })),
    ...NAV_KEYS.map((k) => key(k)),
    ...mineKeys.map((k, i) => key({ key: k.kind === 'key' ? k.value : null, label: k.label }, 'mine', { 'data-mine': i })),
    ...SYM_KEYS.map((ch) => key({ key: ch, label: ch }, 'sym')),
    ...CTL_KEYS.map((k) => key(k, 'ctl')),
    h('button', { class: 'key add', type: 'button', title: 'Add a key of your own' }, '+'),
  );
}

/** The typing-mode key: a red bolt while live, stacked lines while buffered. */
function paintMode(btn) {
  btn.classList.toggle('live', live);
  btn.setAttribute('aria-pressed', String(live));
  btn.title = live ? 'Live typing — tap to buffer lines instead' : 'Buffered lines — tap for live typing';
  btn.setAttribute('aria-label', btn.title);
  btn.innerHTML = `<svg class="ico"><use href="#i-${live ? 'live' : 'buffer'}" /></svg>`;
  return btn;
}

/** The sheet that makes or edits a custom key; `at` is -1 for a new one. */
function editKey(at) {
  const old = myKeys()[at];
  const field = (props) => h('input', { autocomplete: 'off', autocapitalize: 'off', autocorrect: 'off', spellcheck: 'false', ...props });
  const label = field({ value: old?.label || '', placeholder: 'Label on the key', maxlength: '8' });
  const kind = h(
    'select',
    {},
    [['type', 'Type into the line'], ['run', 'Type and press Enter'], ['key', 'Press a key']].map(([v, t]) =>
      h('option', { value: v, text: t, selected: (old?.kind || 'type') === v }),
    ),
  );
  const value = field({ value: old && old.kind !== 'key' ? old.value : '', placeholder: 'git status' });
  const named = h(
    'select',
    {},
    NAMED.map((k) => h('option', { value: k.key, text: `${k.label}  ${k.key}`, selected: old?.kind === 'key' && old.value === k.key })),
  );
  const sync = () => {
    value.hidden = kind.value === 'key';
    named.hidden = kind.value !== 'key';
  };
  kind.onchange = sync;
  sync();
  const done = () => {
    el('modal').hidden = true;
  };
  const save = () => {
    const what = kind.value === 'key' ? named.value : value.value;
    if (!what) return value.focus();
    const list = myKeys();
    list.splice(at < 0 ? list.length : at, at < 0 ? 0 : 1, {
      label: label.value.trim() || what.trim().slice(0, 8),
      kind: kind.value,
      value: what,
    });
    done();
    saveMyKeys(list);
  };
  el('modal-card').replaceChildren(
    h('header', { class: 'mono', text: old ? 'Edit key' : 'New key' }),
    label,
    kind,
    value,
    named,
    h(
      'footer',
      {},
      h('button', { text: 'Cancel', onclick: done }),
      h('button', { class: 'accent', text: 'Save', onclick: save }),
    ),
  );
  el('modal').hidden = false;
  label.focus();
}

function remember(line, files = []) {
  const command = line.trim();
  if (!command) return;
  history = [command, ...history.filter((other) => other !== command)].slice(0, HISTORY_MAX);
  localStorage.setItem('taix-history', JSON.stringify(history));
  if (files.length) lineFiles[command] = files;
  lineFiles = Object.fromEntries(Object.entries(lineFiles).filter(([line]) => history.includes(line)));
  localStorage.setItem('taix-line-files', JSON.stringify(lineFiles));
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
  // An `@name` being typed: the project's files that match, ahead of history.
  if (found?.entries.length) {
    sugg.replaceChildren(
      ...found.entries.map((f) => h('button', { class: 'sug mono', type: 'button', dataset: { pick: f.path } }, `@${f.rel}`)),
    );
    sugg.hidden = false;
    return;
  }
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
