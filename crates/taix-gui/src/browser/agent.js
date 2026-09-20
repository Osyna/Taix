// agent.js - injected at document-start in the main world.
// Every function returns JSON-serializable values; the GTK side always gets
// a stringified result. Keep this dependency-free and readable - no bundler,
// no framework, one file that an agent can understand if it asks.

(function() {
    'use strict';
    
    let refCounter = 0;
    let refMap = new Map();
    const consoleBuffer = [];
    const MAX_CONSOLE = 200;
    
    // Wrap console methods to capture output
    const originalLog = console.log;
    const originalInfo = console.info;
    const originalWarn = console.warn;
    const originalError = console.error;
    
    function captureConsole(level, args) {
        const text = Array.from(args).map(a => {
            try { return typeof a === 'object' ? JSON.stringify(a) : String(a); }
            catch { return String(a); }
        }).join(' ');
        
        const entry = { level, text, source: '', line: 0 };
        
        // Try to get stack trace for source location
        try {
            const stack = new Error().stack;
            const match = stack.split('\n')[3]?.match(/([^/]+):(\d+):\d+/);
            if (match) {
                entry.source = match[1];
                entry.line = parseInt(match[2], 10);
            }
        } catch {}
        
        consoleBuffer.push(entry);
        if (consoleBuffer.length > MAX_CONSOLE) consoleBuffer.shift();
    }
    
    console.log = function(...args) { captureConsole('log', args); originalLog.apply(console, args); };
    console.info = function(...args) { captureConsole('info', args); originalInfo.apply(console, args); };
    console.warn = function(...args) { captureConsole('warn', args); originalWarn.apply(console, args); };
    console.error = function(...args) { captureConsole('error', args); originalError.apply(console, args); };
    
    window.addEventListener('error', (e) => {
        const entry = {
            level: 'error',
            text: e.message || String(e),
            source: e.filename || '',
            line: e.lineno || 0
        };
        consoleBuffer.push(entry);
        if (consoleBuffer.length > MAX_CONSOLE) consoleBuffer.shift();
    });
    
    window.addEventListener('unhandledrejection', (e) => {
        const entry = {
            level: 'error',
            text: String(e.reason || e),
            source: '',
            line: 0
        };
        consoleBuffer.push(entry);
        if (consoleBuffer.length > MAX_CONSOLE) consoleBuffer.shift();
    });
    
    // A headless window has no layout: its viewport is 0x0 and every
    // rectangle is empty. Geometry is then meaningless rather than false,
    // so the visibility rules that depend on it are skipped.
    function laidOut() {
        return window.innerWidth > 0 && window.innerHeight > 0;
    }

    function isVisible(el) {
        const style = window.getComputedStyle(el);
        if (style.display === 'none' || style.visibility === 'hidden') return false;
        if (parseFloat(style.opacity) === 0) return false;
        if (!laidOut()) return true;
        const rect = el.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
    }

    function inViewport(el) {
        if (!laidOut()) return true;
        const rect = el.getBoundingClientRect();
        return rect.top < window.innerHeight && rect.bottom > 0 &&
               rect.left < window.innerWidth && rect.right > 0;
    }

    function isInteractive(el) {
        const tag = el.tagName.toLowerCase();
        if (['a', 'button', 'input', 'select', 'textarea', 'summary'].includes(tag)) return true;
        return el.hasAttribute('onclick') || el.hasAttribute('role') ||
               el.isContentEditable || el.tabIndex >= 0;
    }

    const HEADINGS = ['h1', 'h2', 'h3', 'h4', 'h5', 'h6'];
    const LANDMARKS = ['main', 'navigation', 'banner', 'contentinfo', 'complementary',
                       'region', 'article', 'search', 'form', 'dialog', 'alert'];

    function isStructural(el) {
        if (HEADINGS.includes(el.tagName.toLowerCase())) return true;
        const role = el.getAttribute('role');
        return !!role && LANDMARKS.includes(role);
    }

    function clean(text) {
        return (text || '').replace(/\s+/g, ' ').trim().slice(0, 80);
    }

    // The text this element carries itself, not the text of everything
    // inside it: without that distinction a page is reported once per
    // ancestor and the tree is mostly repetition.
    function ownText(el) {
        let text = '';
        for (const node of el.childNodes) {
            if (node.nodeType === Node.TEXT_NODE) text += node.nodeValue;
        }
        return clean(text);
    }

    function accessibleName(el) {
        const label = el.getAttribute('aria-label');
        if (label) return clean(label);
        const tag = el.tagName;
        if (tag === 'INPUT' || tag === 'TEXTAREA') {
            return clean(el.value || el.placeholder || el.name || '');
        }
        if (tag === 'SELECT') return clean(el.options[el.selectedIndex]?.text || el.name || '');
        if (tag === 'IMG') return clean(el.alt);
        return clean(el.innerText || el.textContent || el.title || '');
    }

    function role(el) {
        const explicit = el.getAttribute('role');
        if (explicit) return explicit;
        const tag = el.tagName.toLowerCase();
        if (tag === 'a') return el.hasAttribute('href') ? 'link' : 'generic';
        if (tag === 'input') return `input:${el.type || 'text'}`;
        if (HEADINGS.includes(tag)) return tag;
        if (['button', 'select', 'textarea', 'summary'].includes(tag)) return tag;
        if (el.isContentEditable) return 'textbox';
        return tag;
    }

    // What the element is, what it says, and the state an agent has to know
    // before deciding to touch it.
    function getLabel(el) {
        let line = `${role(el)} "${accessibleName(el)}"`;
        if (el.disabled) line += ' [disabled]';
        if (el.checked) line += ' [checked]';
        if (el.getAttribute('aria-expanded')) line += ` [expanded=${el.getAttribute('aria-expanded')}]`;
        return line;
    }

    function snapshot(opts) {
        opts = opts || {};
        const needle = opts.filter ? opts.filter.toLowerCase() : null;
        const max = opts.max || 150;
        refCounter = 0;
        refMap.clear();
        const lines = [];
        let truncated = false;

        // `path` carries the ancestors that have not been printed, so a
        // filtered result still says which dialog or form the match is in.
        function emit(el, depth, path) {
            if (lines.length >= max) {
                truncated = true;
                return false;
            }
            for (const [ancestor, at] of path) {
                lines.push(`${'  '.repeat(at)}- ${getLabel(ancestor)}`);
            }
            path.length = 0;
            const ref = `e${++refCounter}`;
            refMap.set(ref, el);
            lines.push(`${'  '.repeat(depth)}- ${getLabel(el)} [ref=${ref}]`);
            return true;
        }

        // What a filter matches against: a control's name, a heading's
        // text, otherwise only the words the element carries itself. The
        // alternative - the whole subtree's text - makes `body` match
        // everything and buries the node that was actually asked for.
        function matchable(el) {
            const tag = el.tagName.toLowerCase();
            const control = ['a', 'button', 'input', 'select', 'textarea', 'summary'];
            return control.includes(tag) || HEADINGS.includes(tag)
                ? `${role(el)} ${accessibleName(el)}`
                : `${role(el)} ${ownText(el)}`;
        }

        function walk(el, depth, path) {
            if (truncated) return;
            if (!isVisible(el)) return;
            const interactive = isInteractive(el);
            const structural = isStructural(el);
            const says = ownText(el).length > 2;
            const wanted = needle
                ? matchable(el).toLowerCase().includes(needle)
                : interactive || structural || (says && inViewport(el));
            let inner = depth;
            if (wanted) {
                if (emit(el, depth, path)) inner = depth + 1;
            } else if (structural || interactive) {
                // Held back rather than dropped: printed only if something
                // below it matches.
                path = path.concat([[el, depth]]);
                inner = depth + 1;
            }
            for (const child of el.children) {
                walk(child, inner, path);
            }
        }

        walk(document.body, 0, []);
        return {
            url: window.location.href,
            title: document.title,
            tree: lines.join('\n'),
            lines: lines.length,
            truncated,
        };
    }
    
    function resolveRef(ref) {
        const el = refMap.get(ref);
        if (!el || !document.contains(el)) {
            throw new Error(`ref ${ref} is stale, snapshot again`);
        }
        return el;
    }
    
    async function act(opts) {
        const action = opts.action;
        // A key press and a scroll are about the page unless an element is
        // named; everything else needs one.
        const loose = action === 'key' || action === 'scroll';
        if (!opts.ref && !loose) throw new Error('this action needs a ref from the last snapshot');
        const el = opts.ref
            ? resolveRef(opts.ref)
            : (action === 'key' ? document.activeElement || document.body : document.scrollingElement || document.body);

        let did = '';
        if (action === 'click') {
            el.scrollIntoView({ block: 'center', behavior: 'auto' });
            await new Promise(r => setTimeout(r, 50));
            
            const rect = el.getBoundingClientRect();
            const x = rect.left + rect.width / 2;
            const y = rect.top + rect.height / 2;
            
            el.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: x, clientY: y }));
            el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, clientX: x, clientY: y }));
            el.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, clientX: x, clientY: y }));
            el.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, clientX: x, clientY: y }));
            el.dispatchEvent(new MouseEvent('click', { bubbles: true, clientX: x, clientY: y }));
            
            did = `clicked ${getLabel(el)}`;
        } else if (action === 'hover') {
            const rect = el.getBoundingClientRect();
            const x = rect.left + rect.width / 2;
            const y = rect.top + rect.height / 2;
            
            el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true, clientX: x, clientY: y }));
            el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: true, clientX: x, clientY: y }));
            
            did = `hovered ${getLabel(el)}`;
        } else if (action === 'type') {
            const text = opts.text || '';
            const field = el.tagName === 'INPUT' || el.tagName === 'TEXTAREA';
            // Typing into whatever was pointed at used to replace its text,
            // which silently deleted the form the agent was filling in.
            if (!field && !el.isContentEditable) {
                throw new Error(`${role(el)} is not a text field; snapshot and pick the input`);
            }
            el.focus();
            if (field) {
                // Through the native setter, or a framework that watches the
                // property never learns the value changed.
                const proto = el.tagName === 'INPUT'
                    ? window.HTMLInputElement.prototype
                    : window.HTMLTextAreaElement.prototype;
                const set = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
                if (set) { set.call(el, text); } else { el.value = text; }
            } else {
                el.textContent = text;
            }
            el.dispatchEvent(new InputEvent('input', { bubbles: true, data: text }));
            el.dispatchEvent(new Event('change', { bubbles: true }));
            if (opts.submit) {
                for (const type of ['keydown', 'keypress', 'keyup']) {
                    el.dispatchEvent(new KeyboardEvent(type, { key: 'Enter', bubbles: true }));
                }
                if (el.form) el.form.requestSubmit?.();
            }
            const shown = text.length > 20 ? `${text.slice(0, 20)}...` : text;
            did = `typed "${shown}" into ${getLabel(el)}`;
        } else if (action === 'key') {
            const key = opts.key || 'Enter';
            el.focus();
            el.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
            el.dispatchEvent(new KeyboardEvent('keypress', { key, bubbles: true }));
            el.dispatchEvent(new KeyboardEvent('keyup', { key, bubbles: true }));
            did = `pressed ${key} on ${getLabel(el)}`;
        } else if (action === 'select') {
            if (el.tagName !== 'SELECT') throw new Error('select action requires a <select> element');
            const values = opts.values || [];
            for (let i = 0; i < el.options.length; i++) {
                const opt = el.options[i];
                opt.selected = values.includes(opt.value);
            }
            el.dispatchEvent(new Event('change', { bubbles: true }));
            did = `selected [${values.join(', ')}] in ${getLabel(el)}`;
        } else if (action === 'scroll') {
            const dx = opts.dx || 0;
            const dy = opts.dy || 0;
            el.scrollBy(dx, dy);
            did = `scrolled by (${dx}, ${dy})`;
        } else {
            throw new Error(`unknown action: ${action}`);
        }
        
        return {
            ok: true,
            url: window.location.href,
            title: document.title,
            did
        };
    }
    
    function waitFor(opts) {
        const { text, gone, url } = opts;
        const ms = opts.ms || 5000;
        const start = Date.now();
        // Nothing to watch for is a plain wait: "give the page a second"
        // is a thing an agent legitimately wants.
        const idle = !text && !gone && !url;
        return new Promise((resolve, reject) => {
            const done = () => resolve({
                ok: true,
                url: window.location.href,
                title: document.title,
                waited_ms: Date.now() - start,
            });
            if (idle) {
                setTimeout(done, ms);
                return;
            }
            const check = () => {
                const body = document.body ? document.body.innerText : '';
                // Every condition given must hold: "this appeared AND that
                // went" is one wait, not two races.
                let met = true;
                if (text) met = met && body.includes(text);
                if (gone) met = met && !body.includes(gone);
                if (url) {
                    const pattern = url.replace(/[.+?^${}()|[\]\\]/g, '\\$&')
                        .replace(/\*\*/g, '\u0000').replace(/\*/g, '[^/]*')
                        .replace(/\u0000/g, '.*');
                    met = met && new RegExp(pattern).test(window.location.href);
                }
                if (met) {
                    done();
                } else if (Date.now() - start > ms) {
                    const what = [
                        text && `"${text}"`,
                        gone && `"${gone}" to go`,
                        url && `url ${url}`,
                    ].filter(Boolean).join(' and ');
                    reject(new Error(`timed out after ${ms}ms waiting for ${what}`));
                } else {
                    setTimeout(check, 100);
                }
            };
            check();
        });
    }
    
    function format(entry) {
        return entry.source
            ? `${entry.level}: ${entry.text} @${entry.source}:${entry.line}`
            : `${entry.level}: ${entry.text}`;
    }

    function getConsole(opts) {
        const entries = opts && opts.clear ? consoleBuffer.splice(0) : consoleBuffer.slice();
        return entries.map(format);
    }

    window.__taix = {
        snapshot,
        act,
        waitFor,
        // Named `resolve` for the host's `eval` with a ref: it hands the
        // function the element the last snapshot numbered.
        resolve: resolveRef,
        console: getConsole,
    };
})();
