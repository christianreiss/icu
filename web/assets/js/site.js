/* ICU — icu.uggs.io
   German is the source language of index.html; EN below overrides it by key.
   No third-party requests. All DOM built from manifest data uses textContent. */
(() => {
'use strict';

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const store = {
  get(k) { try { return localStorage.getItem(k); } catch { return null; } },
  set(k, v) { try { localStorage.setItem(k, v); } catch { /* private mode */ } },
};

/* -------------------------------------------------------------- reveal */

if ('IntersectionObserver' in window) {
  const io = new IntersectionObserver(entries => {
    entries.forEach(en => { if (en.isIntersecting) { en.target.classList.add('in'); io.unobserve(en.target); } });
  }, { threshold: 0.12, rootMargin: '0px 0px -6% 0px' });
  /* hide-until-reveal is armed only now, so a failing or blocked script leaves everything visible;
     content already on screen is shown right away to avoid a flash */
  const vh = innerHeight;
  $$('.reveal').forEach(el => { if (el.getBoundingClientRect().top < vh) el.classList.add('in'); });
  document.documentElement.classList.add('reveal-ready');
  $$('.reveal:not(.in)').forEach(el => {
    const sibs = [...el.parentElement.children].filter(c => c.classList.contains('reveal'));
    el.style.setProperty('--d', Math.min(sibs.indexOf(el), 6));
    io.observe(el);
  });
}

/* ---------------------------------------------------------------- i18n */

const EN = {
  skip: 'Skip to content',
  'nav.label': 'Main navigation', 'nav.features': 'Features', 'nav.views': 'Screenshots', 'nav.start': 'Get started', 'nav.limits': 'On purpose not', 'nav.download': 'Download', 'nav.src': 'Source code on GitHub', 'motion.label': 'Pause decorative animations',
  'hero.tag': 'Room 01 · 1 server · 0 channels',
  'hero.title': 'One crew.<br>One room.<br><em>No&nbsp;noise.</em>',
  'hero.lead': 'ICU is what is left of Mumble and TeamSpeak once you strip out everything a fixed group does not need. In return you get Opus voice quality, echo and noise suppression, a camera overlay for your game and mutual TLS instead of a password.',
  'hero.cta': 'Download', 'hero.cta2': 'See the features', 'hero.platforms': 'Linux (Fedora, Debian) &amp; Windows 10/11', 'hero.src': 'Source code',
  'hero.eye': 'Transmit demo: press and hold', 'hero.tx': 'Transmitting', 'hero.rx': 'Listening', 'hero.hint': '↓ hold the eye = push-to-talk',
  'hero.alt2': 'ICU main window: connected, room list with participants, push-to-talk and control buttons',
  'idx.principle': 'Principle',
  'p.h': 'Leaving things out is the feature.',
  'p.p1': 'No server browser, no channel hierarchy, no roles beyond admin. A fixed group does not need them, and whatever is missing cannot break or be misconfigured.',
  'p.p2': 'Access is a certificate, not a string you hand around: you get an invitation, drop it into the window, pick your name — registered.',
  'p.s1': 'Room', 'p.s2': 'Channels', 'p.s3': 'Passwords', 'p.s4': 'per audio frame',
  'idx.features': 'Features',
  'f.h': 'Only what is really needed.<br><span class="dim">Perfected for that.</span>',
  'f.audio.h': 'Audio',
  'f.audio.p': 'Opus 48&nbsp;kHz mono, adaptive jitter buffer with FEC and PLC, WebRTC AEC3 echo suppression, RNNoise noise suppression and automatic microphone and playback levels.',
  'f.talk.h': 'Talking',
  'f.talk.p': 'Push-to-talk or voice activation, bindable globally — mouse buttons included. With release delay and transmit cue.',
  'f.talk.k': 'Mouse 4',
  'f.people.h': 'Participants',
  'f.people.p': 'Speaking indicator plus mute, deafen and camera status. Volume per person from 0 to 200&nbsp;%, remembered for good.',
  'f.cam.h': 'Camera &amp; overlay',
  'f.cam.p': 'The overlay is its own transparent always-on-top window above borderless games: click-through, GPU-rendered, with DPI-scaled speaker highlighting. No render hooking.',
  'f.id.h': 'Identity, not passwords',
  'f.id.p': 'Mutual TLS with a CA that belongs to your server. You join with an invitation bundle (<code>.icu</code> file or text). Your certificate is visible, and the passport moves your whole identity to another device.',
  'f.id.c1': 'CA signature verified',
  'f.admin.p': 'Invite, promote, ban, mute or deafen, kick — in the client or through the server CLI and shell.',
  'f.ops.h': 'Operations',
  'f.ops.p': 'One static server binary, automatic reconnect with backoff, tray icon and a setup assistant on first launch.',
  'idx.views': 'Screenshots',
  'v.h': 'This is what it looks like.', 'v.label': 'Screenshots',
  'v.t1': 'Main window', 'v.t2': 'Camera', 'v.t3': 'Overlay', 'v.t4': 'Invitation', 'v.t5': 'Connection path',
  'v.1.h': 'One room, everybody in it.',
  'v.1.alt': 'ICU main window: connected to icu.example.org, six people in the room, Anna speaking, icons for muted, deafened and camera, push-to-talk and control buttons at the bottom',
  'v.1.a': 'Room list with speaker highlight, signal strength and mute, deafen and camera status per person',
  'v.1.b': 'Push-to-talk with its key shown, and the control buttons at the bottom of the window',
  'v.1.c': 'The tray icon shows connected, muted and deafened',
  'v.1.d': 'Setup assistant on first launch: devices, talking, sound',
  'v.2.alt': 'Earlier version: ICU with two participants, speaking indicator, camera status and signal strength',
  'v.2.note': 'Image from an earlier version of the app',
  'v.2.h': 'Video call with camera tiles.',
  'v.2.a': 'Speaking indicator, signal strength and camera status per participant',
  'v.2.b': 'Camera on and off with a preview first; H.264 via openh264',
  'v.2.c': 'Video is only sent while somebody is watching',
  'v.3.note': 'Schematic, tile from an earlier version of the app, above a game window',
  'v.3.h': 'Overlay in your game.',
  'v.3.a': 'Click-through and always on top, with speaker highlight and camera image',
  'v.3.b': 'A window of its own instead of render hooking — so borderless games only, exclusive fullscreen covers it',
  'v.3.c': 'Linux: Wayland layer-shell (KDE, wlroots); not available on GNOME',
  'v.4.alt': 'Dialog “Connect to ICU”: server, address and CA fingerprint of the invitation, a field for your name, buttons Back and Join',
  'v.4.h': 'An invitation instead of a password.',
  'v.4.a': 'Paste the invitation or open the <code>.icu</code> file with a double-click',
  'v.4.b': 'Server, address and CA fingerprint are shown before you join',
  'v.4.c': 'You choose your name; it ends up in your certificate',
  'v.5.alt1': 'Connection details, “Who” tab, dark theme: participant list with latency, packet loss and status per person',
  'v.5.alt2': 'Connection details, “Who” tab, light theme',
  'v.5.dark': 'Dark', 'v.5.light': 'Light',
  'v.5.h': 'Who talks to whom — and what it rests on.',
  'v.5.a': 'Latency to the server and packet loss (in ‰, measured at your own jitter buffer) per participant',
  'v.5.b': 'More tabs: Server, Network, Crypto, Audio and Video, plus “Copy report”',
  'v.5.c': 'Moderation for admins: mute, deafen, kick, ban',
  'v.5.d': 'Honestly labelled: both legs are transport-encrypted, and the server can read the media it relays',
  'idx.start': 'Get started',
  's.h': 'Three steps to the first word.',
  's.1.h': 'Set up the server',
  's.1.p': 'One package, one setup dialog. You only need to open a single UDP port (default 4711).',
  's.2.h': 'Issue an invitation',
  's.2.p': 'Whoever registers first becomes admin. Every invitation can be redeemed once and has a time limit.',
  's.3.h': 'Install the client, join',
  's.3.p': 'Paste the invitation or drag the file into the window, pick a name, hit “Join”. The assistant sets up microphone, talk key and sound.',
  copy: 'Copy',
  'idx.limits': 'On purpose not',
  'n.h': 'What is missing is deliberate.',
  'n.p': 'Every line here is a decision. If you need one of them, take Mumble or TeamSpeak instead — and that is fine.',
  'n.1': 'Channels &amp; server browser', 'n.2': 'Permission management (except admin)', 'n.3': 'Text chat &amp; file transfer', 'n.4': 'Bots &amp; plugins', 'n.5': 'Mobile clients &amp; web client', 'n.6': 'Overlay in exclusive fullscreen',
  'n.q': 'Only the features that are really needed. Perfected for that.',
  'idx.trust': 'Trust',
  't.1.h': 'Your own CA, no password',
  't.1.p': 'Every server runs its own CA. The invitation carries the trust anchor, your client certificate is your login. On top, every audio datagram carries an HMAC tag: without the com key the port stays silent.',
  't.2.h': 'Nothing behind your back',
  't.2.p': 'The connection path shows who talks to whom over what. The server is a plain relay: both legs are encrypted with QUIC/TLS&nbsp;1.3, but it can read the media it forwards — which is why your group runs it itself.',
  't.3.h': 'Open and verifiable',
  't.3.p': 'Apache-2.0. Every release carries <code>SHA256SUMS</code> and one <code>BUILDINFO</code> per build with commit and compiler. The packages ship THIRD-PARTY-LICENSES for the bundled third-party code. Source code and releases are on <a href="https://github.com/christianreiss/icu">GitHub</a>.',
  'idx.download': 'Download',
  'd.h': 'Get ICU.',
  'd.loading': 'Loading package list …', 'd.archive': 'Release archive',
  'd.status.h': 'Status',
  'd.status.p': 'Early version: server, protocol and the Linux and Windows clients are implemented. Still open: testing on physical Windows target hardware (acoustic echo cancellation, gaming mouse, mixed DPI) and the portal hotkey on KDE.',
  'd.win.h': 'Windows &amp; SmartScreen',
  'd.win.p': 'The Windows builds are unsigned. On first launch: “More info” → “Run anyway”. Where Smart App Control is switched on, Windows blocks the file.',
  'd.all': 'All packages', 'd.kind': 'Client or server', 'd.client': 'Client', 'd.server': 'Server',
  'd.verify.h': 'Verify your download',
  'd.verify.p': 'The SHA-256 sums are listed next to every package and in <a id="sums-link" href="icu/">SHA256SUMS</a>. In the download folder:',
  'foot.tag': 'A minimal voice system for a fixed group.',
  'foot.legal': 'License: Apache-2.0. This page loads nothing from third parties: no trackers, no external fonts (Red Hat Display/Text and Source Code Pro, SIL OFL, self-hosted).',
  'foot.src': 'Source code on GitHub', 'foot.archive': 'Release archive', 'foot.top': 'Back to top',
};

/* strings built at runtime */
const RT = {
  de: {
    title: 'ICU – Voice für eine feste Gruppe',
    desc: 'ICU ist ein minimales Voice-System für eine feste Gruppe: ein Server, ein Raum, keine Channels. Opus, Echo- und Rauschunterdrückung, Kamera-Overlay fürs Spiel, mutual TLS statt Passwort. Für Linux und Windows.',
    kicker: 'Empfohlen für dein System', get: 'Herunterladen', alt: 'Stattdessen: Portable ZIP',
    forWin: 'ICU {v} für Windows', forLinux: 'ICU {v} für Linux', forOther: 'ICU {v}',
    subWin: 'Setup pro Benutzer: Startmenü-Eintrag, Deinstallation über die Systemsteuerung, keine Administratorrechte.',
    subLinux: 'Wähle deine Distribution — die Installation läuft über den Paketmanager.',
    subOther: 'ICU gibt es für Linux und Windows; ein Client für dieses Gerät existiert nicht. Server und alle Pakete stehen unten.',
    unsigned: 'Unsigniert. SmartScreen: „Weitere Informationen“ → „Trotzdem ausführen“.',
    dlOther: 'Alle Pakete ansehen', winSetup: 'Windows 10/11 · Setup', winZip: 'Windows 10/11 · Portable ZIP',
    fmt: { setup: 'Setup', zip: 'ZIP', rpm: 'RPM', deb: 'DEB' },
    kickerPick: 'Wähle deine Distribution', kickerChosen: 'Deine Auswahl',
    pickHint: 'Wir konnten deine Distribution nicht erkennen. Wähle sie aus, dann erscheinen Paket und Installationsbefehl.',
    copied: 'Kopiert', copyFail: 'Kopieren nicht möglich – Text markiert, bitte manuell kopieren (Strg+C)', error: 'Die Paketliste konnte nicht geladen werden.', empty: 'Noch keine Pakete veröffentlicht.',
    version: 'Version {v}', build: 'Build aus Commit {c}', dirty: 'Arbeitsverzeichnis mit uncommitteten Änderungen', sig: 'signiert: SHA256SUMS.sig',
    heroCta: { windows: 'Download für Windows', linux: 'Download für Linux', other: 'Download' },
    dlBtn: 'Herunterladen', x64: 'x86_64', mb: 'MB',
  },
  en: {
    title: 'ICU – Voice for a fixed group',
    desc: 'ICU is a minimal voice system for a fixed group: one server, one room, no channels. Opus, echo and noise suppression, a camera overlay for your game, mutual TLS instead of a password. For Linux and Windows.',
    kicker: 'Recommended for your system', get: 'Download', alt: 'Or: portable ZIP',
    forWin: 'ICU {v} for Windows', forLinux: 'ICU {v} for Linux', forOther: 'ICU {v}',
    subWin: 'Per-user installer: Start menu entry, uninstall through Settings, no administrator rights.',
    subLinux: 'Pick your distribution — installation goes through your package manager.',
    subOther: 'ICU exists for Linux and Windows; there is no client for this device. The server and all packages are listed below.',
    unsigned: 'Unsigned. SmartScreen: “More info” → “Run anyway”.',
    dlOther: 'See all packages', winSetup: 'Windows 10/11 · Setup', winZip: 'Windows 10/11 · Portable ZIP',
    fmt: { setup: 'Setup', zip: 'ZIP', rpm: 'RPM', deb: 'DEB' },
    kickerPick: 'Choose your distribution', kickerChosen: 'Your selection',
    pickHint: 'We could not detect your distribution. Select it and the matching package and install command will appear.',
    copied: 'Copied', copyFail: 'Copy not possible – text selected, please copy manually (Ctrl+C)', error: 'The package list could not be loaded.', empty: 'No packages published yet.',
    version: 'Version {v}', build: 'Built from commit {c}', dirty: 'working tree with uncommitted changes', sig: 'signed: SHA256SUMS.sig',
    heroCta: { windows: 'Download for Windows', linux: 'Download for Linux', other: 'Download' },
    dlBtn: 'Download', x64: 'x86_64', mb: 'MB',
  },
};

let lang = 'de';
const rt = () => RT[lang];
const fill = (s, o) => s.replace(/\{(\w+)\}/g, (_, k) => o[k]);

/* remember the German source text once */
$$('[data-i18n]').forEach(el => { el.dataset.de = el.innerHTML; });
$$('[data-i18n-attr]').forEach(el => {
  el.dataset.i18nAttr.split(';').forEach(pair => {
    const [attr] = pair.split(':');
    el.dataset['de_' + attr.replace(/-/g, '_')] = el.getAttribute(attr);
  });
});

function setLang(next, persist = true) {
  lang = next === 'en' ? 'en' : 'de';
  $$('[data-i18n]').forEach(el => {
    const v = lang === 'en' ? EN[el.dataset.i18n] : el.dataset.de;
    if (v != null) el.innerHTML = v;
  });
  $$('[data-i18n-attr]').forEach(el => {
    el.dataset.i18nAttr.split(';').forEach(pair => {
      const [attr, key] = pair.split(':');
      const v = lang === 'en' ? EN[key] : el.dataset['de_' + attr.replace(/-/g, '_')];
      if (v != null) el.setAttribute(attr, v);
    });
  });
  document.documentElement.lang = lang;
  document.title = rt().title;
  const meta = $('meta[name=description]');
  if (meta) meta.setAttribute('content', rt().desc);
  $$('.lang button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.lang === lang)));
  if (persist) store.set('icu-lang', lang);
  renderDownloads();
}

$$('.lang button').forEach(b => b.addEventListener('click', () => setLang(b.dataset.lang)));

/* ------------------------------------------------------------ downloads */

let manifest = null;
const ui = { kind: 'client', pick: null };

function h(tag, props = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === 'class') el.className = v;
    else if (k === 'text') el.textContent = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (v != null && v !== false) el.setAttribute(k, v);
  }
  kids.flat().forEach(c => c != null && el.append(c));
  return el;
}
const icon = id => {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  s.setAttribute('class', 'ic');
  const u = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  u.setAttribute('href', '#' + id);
  s.append(u);
  return s;
};

function detectOS() {
  const ua = navigator.userAgent || '';
  const plat = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
  if (/android|iphone|ipad|ipod|cros/i.test(ua)) return { os: 'other' };
  if (/win/i.test(plat) || /Windows/.test(ua)) return { os: 'windows' };
  if (/mac/i.test(plat)) return { os: 'other' };
  if (/linux|x11/i.test(plat + ' ' + ua)) {
    if (/Fedora/i.test(ua)) return { os: 'linux', distro: 'fedora' };
    if (/Debian|Ubuntu/i.test(ua)) return { os: 'linux', distro: 'debian' };
    return { os: 'linux' };
  }
  return { os: 'other' };
}

const fmtSize = b => (b / 1048576).toLocaleString(lang, { maximumFractionDigits: 1, minimumFractionDigits: 1 }) + ' ' + rt().mb;
const url = f => manifest.base + f.name;
const shortHash = s => s.slice(0, 12) + '…' + s.slice(-8);

function pkgLabel(f) {
  if (f.kind === 'client') {
    if (f.os === 'windows') return f.format === 'setup' ? rt().winSetup : rt().winZip;
    if (f.os.startsWith('fedora')) return 'Fedora ' + f.os.slice(6);
    if (f.os.startsWith('debian')) return 'Debian ' + f.os.slice(6);
  } else {
    if (f.os === 'linux-rpm') return 'Fedora · AlmaLinux 10+';
    if (f.os === 'linux-deb') return 'Debian 12+';
  }
  return f.name;
}
const distroLabel = f => pkgLabel(f);
const installCmd = f => (f.format === 'rpm' ? 'sudo dnf install ./' : 'sudo apt install ./') + f.name;

/* manual-copy fallback: stays until dismissed (Escape, click elsewhere, focus leaves, a new copy attempt, navigation) */
let manualFail = null;
function clearManual() {
  if (!manualFail) return;
  const m = manualFail; manualFail = null;
  m.el.classList.remove('fail'); delete m.el.dataset.msg;
  if (m.undo) m.undo();
}
document.addEventListener('keydown', e => { if (e.key === 'Escape') clearManual(); });
document.addEventListener('click', e => { if (manualFail && !manualFail.el.contains(e.target)) clearManual(); });
addEventListener('hashchange', clearManual);
addEventListener('pagehide', clearManual);

async function copyText(text, el, sel, reveal) {
  clearManual();
  let ok = false;
  try { await navigator.clipboard.writeText(text); ok = true; }
  catch {
    const t = h('textarea', { 'aria-hidden': 'true' });
    t.value = text; t.style.position = 'fixed'; t.style.opacity = '0';
    document.body.append(t); t.select();
    try { ok = document.execCommand('copy') === true; } catch { ok = false; }
    t.remove();
  }
  if (ok) {
    el.classList.remove('fail'); el.classList.add('done');
    clearTimeout(el._doneTimer);
    el._doneTimer = setTimeout(() => el.classList.remove('done'), 1600);
    return true;
  }
  /* manual fallback: select the visible text and say so */
  clearManual();
  const undo = reveal && reveal();
  if (sel) { const r = document.createRange(); r.selectNodeContents(sel); const g = getSelection(); g.removeAllRanges(); g.addRange(r); }
  el.dataset.msg = rt().copyFail;
  el.classList.add('fail');
  manualFail = { el, undo };
  el.addEventListener('focusout', () => { if (manualFail && manualFail.el === el && document.hasFocus()) clearManual(); }, { once: true });
  return false;
}

function hashButton(f, full) {
  const label = h('span', { text: full ? f.sha256 : shortHash(f.sha256) });
  const b = h('button', { class: 'sha', type: 'button', title: 'SHA-256: ' + f.sha256, 'aria-label': 'SHA-256 ' + f.sha256 }, label, icon('i-copy'));
  b.addEventListener('click', async () => {
    clearManual();   /* restores the short label before it is read below */
    clearTimeout(b._okTimer);
    const old = full ? f.sha256 : shortHash(f.sha256);
    /* on failure show the full hash so the manual-copy selection is the real 64 hex chars */
    const ok = await copyText(f.sha256, b, label, () => {
      label.textContent = f.sha256; b.classList.add('revealed');
      return () => { label.textContent = old; b.classList.remove('revealed'); };
    });
    if (!ok) return;
    label.textContent = rt().copied;
    b._okTimer = setTimeout(() => { label.textContent = old; }, 1600);
  });
  return b;
}

function renderPrimary() {
  const box = $('#dl-primary');
  const clients = manifest.files.filter(f => f.kind === 'client');
  const det = detectOS();
  const v = manifest.version;
  const t = rt();
  box.replaceChildren();

  const kicker = h('p', { class: 'dl-kicker' }, h('span', { class: 'led', 'aria-hidden': 'true' }), t.kicker + ' · ' + (det.os === 'windows' ? 'Windows' : det.os === 'linux' ? 'Linux' : '—'));
  if (det.os === 'windows') {
    const setup = clients.find(f => f.os === 'windows' && f.format === 'setup');
    const zip = clients.find(f => f.os === 'windows' && f.format === 'zip');
    if (setup) {
      box.append(kicker,
        h('h3', { class: 'dl-title', text: fill(t.forWin, { v }) }),
        h('p', { class: 'dl-sub', text: t.subWin }),
        h('p', { class: 'dl-note', text: t.unsigned }),
        h('div', { class: 'dl-actions' },
          h('a', { class: 'btn btn-lg', href: url(setup), download: '' }, icon('i-download'), h('span', { text: t.get + ' · ' + fmtSize(setup.size) })),
          zip ? h('a', { class: 'dl-alt', href: url(zip), download: '', text: t.alt + ' (' + fmtSize(zip.size) + ')' }) : null),
        h('div', { class: 'dl-hash' }, h('span', { text: 'SHA-256' }), hashButton(setup, true)));
      return;
    }
  }
  if (det.os === 'linux') {
    const linux = clients.filter(f => f.os !== 'windows');
    if (linux.length) {
      if (ui.pick && !linux.some(f => f.name === ui.pick)) ui.pick = null;
      /* only a detected distribution preselects a package; otherwise the user must choose */
      if (!ui.pick && det.distro) {
        const pref = linux.filter(f => f.os.startsWith(det.distro)).sort((a, b) => b.os.localeCompare(a.os))[0];
        if (pref) ui.pick = pref.name;
      }
      const cur = linux.find(f => f.name === ui.pick);
      kicker.lastChild.textContent = (!cur ? t.kickerPick : det.distro && cur.os.startsWith(det.distro) ? t.kicker : t.kickerChosen) + ' · Linux';
      const seg = h('div', { class: 'seg dl-distros', role: 'group', 'aria-label': 'Distribution' },
        linux.map(f => h('button', { type: 'button', 'aria-pressed': String(f.name === ui.pick), text: distroLabel(f), onclick: () => { ui.pick = f.name; renderPrimary(); $$('.dl-distros button')[linux.indexOf(f)].focus(); } })));
      if (!cur) {
        box.append(kicker,
          h('h3', { class: 'dl-title', text: fill(t.forLinux, { v }) }),
          h('p', { class: 'dl-sub', text: t.pickHint }),
          seg);
        return;
      }
      box.append(kicker,
        h('h3', { class: 'dl-title', text: fill(t.forLinux, { v }) }),
        h('p', { class: 'dl-sub', text: t.subLinux }),
        seg,
        h('div', { class: 'dl-actions' },
          h('a', { class: 'btn btn-lg', href: url(cur), download: '' }, icon('i-download'), h('span', { text: t.get + ' · ' + fmtSize(cur.size) }))),
        h('div', { class: 'code' }, h('button', { class: 'copy', type: 'button', 'aria-label': lang === 'en' ? EN.copy : 'Kopieren', onclick: e => copyText(installCmd(cur), e.currentTarget, e.currentTarget.nextElementSibling) }, icon('i-copy')), h('pre', {}, h('code', { text: installCmd(cur) }))),
        h('div', { class: 'dl-hash' }, h('span', { text: 'SHA-256' }), hashButton(cur, true)));
      return;
    }
  }
  box.append(kicker,
    h('h3', { class: 'dl-title', text: fill(t.forOther, { v }) }),
    h('p', { class: 'dl-sub', text: t.subOther }),
    h('div', { class: 'dl-actions' }, h('a', { class: 'btn btn-lg', href: '#pkgs' }, icon('i-arrow'), h('span', { text: t.dlOther }))));
}

function renderList() {
  const list = $('#pkgs');
  list.replaceChildren();
  const files = manifest.files.filter(f => f.kind === ui.kind);
  if (!files.length) { list.append(h('li', { class: 'dl-empty', text: rt().empty })); return; }
  const rank = f => (f.os === 'windows' ? (f.format === 'setup' ? 0 : 1) : f.os.startsWith('fedora') ? 10 - Number(f.os.slice(6)) / 100 : f.os.startsWith('debian') ? 20 : f.os === 'linux-rpm' ? 0 : 1);
  files.sort((a, b) => rank(a) - rank(b));
  for (const f of files) {
    const win = f.os === 'windows';
    list.append(h('li', { class: 'pkg' },
      h('div', { class: 'pkg-name' }, icon(win ? 'i-win' : 'i-term'),
        h('div', {}, h('strong', { text: pkgLabel(f) }), h('small', { text: f.name }))),
      h('div', { class: 'pkg-meta' },
        h('span', { class: 'pkg-fmt', text: rt().fmt[f.format] || f.format }),
        h('span', { class: 'pkg-size', text: fmtSize(f.size) })),
      hashButton(f, false),
      h('a', { class: 'btn btn-sm btn-ghost', href: url(f), download: '', 'aria-label': rt().dlBtn + ' ' + f.name }, icon('i-download'), h('span', { text: rt().dlBtn }))));
  }
}

function renderMeta() {
  const d = new Date(manifest.published);
  const bits = [fill(rt().version, { v: manifest.version }), d.toLocaleDateString(lang, { year: 'numeric', month: 'long', day: 'numeric' })];
  if (manifest.commit) bits.push(fill(rt().build, { c: manifest.commit.slice(0, 7) }) + (manifest.dirty ? ' (' + rt().dirty + ')' : ''));
  if (manifest.sig) bits.push(rt().sig);
  $('#buildinfo').textContent = bits.join(' · ');
  const sums = $('#sums-link');
  if (sums) sums.href = manifest.base + manifest.sums;
}

function renderDownloads() {
  const o = detectOS().os;
  $('#hero-dl-label').textContent = rt().heroCta[o === 'windows' ? 'windows' : o === 'linux' ? 'linux' : 'other'];
  if (!manifest) return;
  $$('.ver').forEach(el => { el.textContent = manifest.version; });
  $('#hero-version').textContent = 'v' + manifest.version;
  renderPrimary(); renderList(); renderMeta();
}

$$('.seg button[data-kind]').forEach(b => b.addEventListener('click', () => {
  ui.kind = b.dataset.kind;
  $$('.seg button[data-kind]').forEach(x => x.setAttribute('aria-pressed', String(x === b)));
  if (manifest) renderList();
}));

fetch('downloads/latest.json', { cache: 'no-cache' })
  .then(r => { if (!r.ok) throw new Error(r.status); return r.json(); })
  .then(m => { manifest = m; renderDownloads(); })
  .catch(() => {
    const box = $('#dl-primary');
    box.replaceChildren(h('p', { class: 'dl-empty', text: rt().error }, ' ', h('a', { href: 'icu/', text: lang === 'en' ? EN['d.archive'] : 'Release-Archiv' })));
  });

/* copy buttons in static code blocks */
$$('.code .copy').forEach(b => b.addEventListener('click', () => {
  const pre = $('pre', b.parentElement);
  if (pre) copyText(pre.textContent.trim(), b, pre);
}));

/* ----------------------------------------------------------- hero eye */

const stage = $('#stage');
const eye = $('#eye');
const iris = $('.eye-iris', eye);
const reduce = matchMedia('(prefers-reduced-motion: reduce)');
let raf = 0;
addEventListener('pointermove', e => {
  if (e.pointerType === 'touch' || reduce.matches || raf) return;
  raf = requestAnimationFrame(() => {
    raf = 0;
    const r = eye.getBoundingClientRect();
    const dx = e.clientX - (r.left + r.width / 2), dy = e.clientY - (r.top + r.height / 2);
    const a = Math.atan2(dy, dx), m = Math.min(Math.hypot(dx, dy) / 320, 1);
    iris.style.setProperty('--ex', (Math.cos(a) * m * 9).toFixed(2));
    iris.style.setProperty('--ey', (Math.sin(a) * m * 5.5).toFixed(2));
  });
}, { passive: true });

const tx = on => { stage.classList.toggle('tx', on); eye.setAttribute('aria-pressed', String(on)); };
eye.addEventListener('pointerdown', e => { eye.setPointerCapture(e.pointerId); tx(true); });
['pointerup', 'pointercancel', 'lostpointercapture'].forEach(t => eye.addEventListener(t, () => tx(false)));
eye.addEventListener('keydown', e => { if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) { e.preventDefault(); tx(true); } });
eye.addEventListener('keyup', e => { if (e.key === ' ' || e.key === 'Enter') tx(false); });
eye.addEventListener('blur', () => tx(false));

/* pause / play for continuous decorative motion (ticker, rings, equalizer); remembered for this session only */
const motionBtn = $('#motion');
if (motionBtn) {
  const setMotion = paused => {
    document.documentElement.classList.toggle('motion-off', paused);
    motionBtn.setAttribute('aria-pressed', String(paused));
    try { sessionStorage.setItem('icu-motion', paused ? 'off' : 'on'); } catch { /* private mode */ }
  };
  let saved = null;
  try { saved = sessionStorage.getItem('icu-motion'); } catch { /* private mode */ }
  setMotion(saved === 'off');
  motionBtn.addEventListener('click', () => setMotion(motionBtn.getAttribute('aria-pressed') !== 'true'));
}

/* card spotlight */
$$('.card').forEach(c => c.addEventListener('pointermove', e => {
  const r = c.getBoundingClientRect();
  c.style.setProperty('--mx', (e.clientX - r.left) + 'px');
  c.style.setProperty('--my', (e.clientY - r.top) + 'px');
}, { passive: true }));

/* ------------------------------------------------------ screenshot tabs */

const tabs = $$('.tabs [role=tab]');
function selectTab(t, focus) {
  tabs.forEach(x => {
    const on = x === t;
    x.setAttribute('aria-selected', String(on));
    x.tabIndex = on ? 0 : -1;
    $('#' + x.getAttribute('aria-controls')).hidden = !on;
  });
  if (focus) t.focus();
}
tabs.forEach((t, i) => {
  t.addEventListener('click', () => selectTab(t));
  t.addEventListener('keydown', e => {
    const k = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
    if (k) { e.preventDefault(); selectTab(tabs[(i + k + tabs.length) % tabs.length], true); }
    if (e.key === 'Home') { e.preventDefault(); selectTab(tabs[0], true); }
    if (e.key === 'End') { e.preventDefault(); selectTab(tabs[tabs.length - 1], true); }
  });
});
$$('[data-theme-shot]').forEach(b => b.addEventListener('click', () => {
  const frame = b.closest('.frame');
  frame.dataset.theme = b.dataset.themeShot;
  $$('[data-theme-shot]', frame).forEach(x => x.setAttribute('aria-pressed', String(x === b)));
}));

/* ---------------------------------------------------------------- boot */

const saved = store.get('icu-lang');
const pref = saved || ((navigator.languages && navigator.languages[0]) || navigator.language || 'de').slice(0, 2);
setLang(pref === 'de' ? 'de' : 'en', false);
})();
