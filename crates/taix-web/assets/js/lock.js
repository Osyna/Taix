// Face ID unlock. The server is the lock - after five idle minutes it
// refuses everything but this - and this file is how a person opens it:
// a full-screen "Unlock TaiX" that asks the phone's own authenticator to
// sign a challenge. Nothing here decides whether the page may be used.

import { api, h } from './store.js';

const enc = (buf) =>
  btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const dec = (s) =>
  Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));

/** WebAuthn only exists in a secure context: localhost or https. */
export const supported = () => window.isSecureContext && !!window.PublicKeyCredential;

/** `{ registered, locked, cred }` for this device. */
export const state = () => api('/passkey/state');

export async function register() {
  const { challenge, device, name } = await api('/passkey/challenge', {});
  const made = await navigator.credentials.create({
    publicKey: {
      rp: { name: 'TaiX', id: location.hostname },
      user: { id: dec(device), name: 'taix', displayName: name },
      challenge: dec(challenge),
      pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
      authenticatorSelection: {
        authenticatorAttachment: 'platform',
        userVerification: 'required',
        residentKey: 'preferred',
      },
      attestation: 'none',
    },
  });
  await api('/passkey/register', {
    clientDataJSON: enc(made.response.clientDataJSON),
    attestationObject: enc(made.response.attestationObject),
  });
}

export const remove = () => api('/passkey/remove', {});

async function unlock() {
  const { cred } = await state();
  const { challenge } = await api('/passkey/challenge', {});
  const got = await navigator.credentials.get({
    publicKey: {
      challenge: dec(challenge),
      rpId: location.hostname,
      allowCredentials: [{ type: 'public-key', id: dec(cred), transports: ['internal'] }],
      userVerification: 'required',
    },
  });
  const r = got.response;
  await api('/passkey/unlock', {
    id: enc(got.rawId),
    clientDataJSON: enc(r.clientDataJSON),
    authenticatorData: enc(r.authenticatorData),
    signature: enc(r.signature),
  });
}

let screen = null;
let unlocked = () => {};

/** Whether the lock screen is up: the socket must not reconnect into it. */
export const locked = () => screen !== null;

/** What to do once the device opens, which is to reconnect. */
export function onUnlock(fn) {
  unlocked = fn;
}

/** Ask the server, and raise or drop the lock screen to match. */
export async function check() {
  let now;
  try {
    now = await state();
  } catch {
    // Unpaired or unreachable: not this file's business.
    return false;
  }
  if (now.locked) show();
  else hide();
  return now.locked;
}

function hide() {
  screen?.remove();
  screen = null;
}

function show() {
  if (screen) return;
  const err = h('p', { class: 'lock-err' });
  const go = h('button', {
    class: 'lock-go',
    text: 'Unlock',
    onclick: async () => {
      err.textContent = '';
      try {
        await unlock();
        hide();
        unlocked();
      } catch (e) {
        err.textContent = e.name === 'NotAllowedError' ? 'Cancelled.' : e.message;
      }
    },
  });
  screen = h('div', { id: 'lock', class: 'lock' },
    h('div', { class: 'lock-card' },
      h('img', { src: '/icon.svg', alt: '', width: 72, height: 72 }),
      h('h1', { text: 'Unlock TaiX' }),
      go,
      err));
  document.body.append(screen);
}
