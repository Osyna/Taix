# Changelog

## 0.3.1

- Linux packaging: static musl CLI/TUI tarball for any x86_64 Linux; native `taix` and `taix-gui` deb, rpm and Arch packages; GUI archive built on Ubuntu 24.04.
- Release checksums published with the artifacts.
- Version correction: workspace and crates are 0.3.1. The 0.3.0 release was tagged from workspace 0.2.0 and is left as is.
- README: install, uninstall and supported-distribution notes. There is no automatic update mechanism.

## 0.3.0

### A web page that keeps up

- Typing and clicking on the web page no longer lag by seconds on a slow
  link. The page used to receive every pane the desktop showed, whole, about
  17 times a second (≈80 KB/s with two agents spinning), and the server queued
  every frame in the socket, so on a 512 kbit/s link a keystroke took a median
  of 1.1 s to echo and up to 7 s. Each page now gets only the panes it is
  showing, and of those only the rows that changed: about 1–5 KB/s, and 80 ms
  to echo on the same link. A slow page skips stale screens instead of
  queueing them.
- The desktop captures only the panes a browser is looking at, a quiet pane
  only when it printed something (or every 2 s), and a pane the browser is
  typing into at the web's own rate instead of every 400 ms.
- The info rail no longer asks for the job list on every frame: jobs and git
  are polled every 5 s, and the rail and open panels redraw only when the
  session changes, not when a pane prints.
- Typed characters show up at once, dimmed, before the desktop echoes them
  (as mosh does). Nothing is shown on a line until the shell has echoed one
  character back, so a password prompt never displays what you type, and a
  full-screen or mouse-mode program is never predicted.
- Scrolling output costs about one row instead of a screen: the page moves
  the rows it already has. A pane scrolling 20 lines a second now takes
  under 1 KB/s on the wire. The socket is also compressed
  (`permessage-deflate`), rows travel as text plus a style number instead of
  HTML, and only the parts of the session that changed are resent.
- A pane a browser is driving gets its own terminal emulator on the desktop,
  fed from tmux's output stream, so it updates without re-reading the pane
  from tmux.
- A hidden tab or a phone in a pocket receives no pane updates. The page
  opens with the session already filled in and the last screen it showed,
  and static files are revalidated instead of downloaded again.
- The status bar shows the round trip to the desktop and the last
  keystroke's echo time. A tab that becomes visible again reconnects within
  a second if the old connection is dead.
- Mouse clicks on the project list, window tabs and layout buttons act on
  press instead of on release.

### Headless in a Proxmox LXC

- `packaging/proxmox/taix-lxc.sh`, run on a Proxmox VE host, creates a Debian
  13 container running TaiX with no display (headless sway plus
  `taix-host.service`) and prints the web pairing link. Terminals open in zsh
  with oh-my-zsh, and Oh My Pi (`omp`) is installed. Re-run it with the
  container id to update the binaries. Agents survive a service restart.
- With oh-my-zsh under tmux, a driven pane no longer prints each command's
  name in front of its output (`echo hi` showed `echohi`): the screen-style
  title escape (`ESC k … ESC \`) that tmux passes through is now dropped.
- Selecting text in a terminal on the web page works while a program has the
  mouse (omp, Claude Code, htop): Shift+drag selects, as in xterm, and
  Ctrl/Cmd+C copies the selection instead of sending ^C. A selection is no
  longer dropped by the click that ends it or by new output, and typing
  goes back to the terminal.
- Right-click on a terminal opens TaiX's own menu: Copy and Attach for
  a selection, Paste, Open/Copy link, Select all, Copy screen, and Select
  mode, where every drag selects even in a program that takes the mouse
  (Esc leaves it). Shift+right-click still opens the browser's menu. On
  plain http, where the page may not read the clipboard, Paste points to
  Ctrl+V.
- Ctrl+V pastes into a terminal on the web page. It used to send a raw ^V,
  so nothing from the clipboard ever arrived; a Mac keeps Ctrl+V as ^V and
  pastes with Cmd+V. A paste also lands while a selection has focus off the
  input, and a multi-line paste goes in as one bracketed paste.

### Share a window, name your devices

- **Share read-only.** A window's menu on the web page has *Share read-only*: it
  copies a link (`/s/<token>/`) that shows that one window's screen to anyone who
  has it, with no pairing. The link ends after an hour, or when you revoke it
  in Settings → Read-only links; changing the pairing key revokes them all.
  The visitor sees the screen, the window's name and state, and nothing else: no
  files, git or other windows, and the server drops anything it sends back.
- **Named devices.** Each browser has a name ("iPad", "Chrome on Linux"), shown
  in Settings → This device, where you can change it. When one device is typing
  into a window, the others see "driven by iPad", and only that device's keys
  reach the terminal until another one takes it.
- **Send to…** in a window's menu lists the other connected browsers; the one you
  pick gets a toast "build sent from iPad — Open" that switches to the window
  and takes control.

### Sessions you can come back to

- With shell integration on (OSC 133 prompt marks; bash and zsh snippets in the
  README, fish 4.0+ does it itself), TaiX remembers the last 200 commands each
  window ran. The web page has a **History** panel: the shown window's commands
  with ✓/✗ exit status and duration, tap one to copy it, and a box that searches
  every window.
- **↑ ↓** buttons in a web pane's header (and "Previous/Next prompt" in the pane
  menu on a phone) scroll its scrollback to the previous or next command.

### Jobs that wait for something

- A job's schedule can be an event instead of a clock: `on agent-waiting`,
  `on agent-idle`, `on port-up:<port>`, `on job-done:<id>` or
  `on job-failed:<id>`. `taix jobs add … --on port-up:5173` sets one; the
  Automation dialog and the web job form offer the agent ones. The window
  reports each event once, when it happens (a port that was already up when
  TaiX started is not an event), and the job is claimed under the store lock like
  a timed one, so it never runs twice. Two reports within 10 seconds count as one.
- Chains: `job-done:<id>` runs after that job succeeds and `job-failed:<id>`
  after it fails, never both. A loop of jobs naming each other stops after one
  lap.
- Each run keeps what it printed (the last 64 KiB of it, for the last 20 runs).
  On the web page, tap a run in History to read it and press **Run again**.

### Small fixes on the web page

- Attached files show as chips instead of bare paths. On the web page, a photo
  or file added to the line sits over the field as an icon with its name and
  extension; tapping it opens a sheet with an image preview, the full path with
  a copy button, and Remove. The paths go in front of the line when it is sent.
  On the desktop, a pasted image, a pasted or dropped file, or a file inserted
  from the file tree still types its path, and also gets a chip at the bottom of
  the pane. Hover shows the path; a click opens a popover with the preview,
  copy and Remove. Remove backspaces the path off the line while nothing has
  been typed after it. Enter or Ctrl+C clears the chips. Each chip is
  numbered, and a `[1]`, `[2]`, … reference is typed into the line where the
  file went in, so the prose can point at it ("the error is [1], the feature
  [2]"); on the web page the sent line carries `[1] path [2] path` ahead of
  the text, and the field draws each reference as a small card with the
  file's icon and number. Every file has its own colour (golden-angle hues,
  never repeated) for its whole chip and its card. Deleting a reference from
  the web field removes that file.
- The web API can list recent attachments (`GET /api/stash`), serve an image
  from a project or the attachment folder for previews (`GET /api/raw`: png,
  jpg, gif, webp and bmp only), and find project files by name for an
  @-mention picker (`GET /api/find`, up to 30, best match first).
- More ways to attach on the web page, all landing as numbered chips:
  - Paste a screenshot into the field.
  - Type `@` and part of a name to pick a project file.
  - Use **Attach** on a terminal selection, **Attach** in the file viewer
    (attaches the selected lines, or else the whole file), or **Attach to
    line** on a file's ⋮ menu.
  - Use **+ → Recent attachments** to pick from everything attached from the
    phone or the desktop in the last 14 days. The folder deletes files older
    than that.
  - Use the mic to dictate, where "image two" or "file 1" becomes `[2]` or
    `[1]` for a file that is attached.
- What you can do with an attached file on the web page:
  - Tap a `[n]` card in the field to open the file's sheet.
  - From the sheet, insert its reference again at the cursor, or **Draw on
    it** with a red pen or box. Saving puts the marked-up copy in the chip.
  - Drag a chip into the text to place its reference there.
  - Chips survive a reload with the draft. Recalling a line from history
    brings back the files it carried.
  - Files already sent stay as faded **sent** chips, so you can attach them
    again with one tap.
- On a tablet or with live typing, a second attached file is typed as `[2]`,
  not another `[1]`; the count restarts after Enter or ^C.
- The file viewer no longer reloads under you every two seconds while a pane
  is busy, so a selection survives long enough to press **Attach**.
- Desktop: an attached file's `[n] path` is pasted (bracketed paste when the
  program asked for it) instead of typed key by key, so a TUI sees a paste.
- The pane a desktop focus switch seeds no longer paints the next
  default-coloured text in the colour the capture ended on (stray backgrounds
  in ratatui status bars).
- Links in a terminal open in a new tab: a click on an `http(s)://` URL in a
  pane opens it in a new browser window of that project on the desktop, and in
  a new browser tab on the web page. When the program in the pane takes mouse
  clicks, Ctrl+click (Cmd on a Mac) opens the link instead.
- Live typing on a phone: the first key in the key row switches between
  buffered lines (the staged field, sent whole) and live keys (a red bolt),
  where every character goes straight to the terminal so shell completion,
  slash-command menus and TUIs react as you type. Tapping the pane then
  brings up the keyboard. Going live hands any staged text to the shell
  first. The choice is remembered per browser, and a toast says which mode
  you are in.
- A "Reconnecting… last update N s ago" banner shows while the page has lost
  its connection to TaiX, and goes away when it is back.
- Hold ⌘ (Meta) alone for about half a second on a laptop or iPad keyboard to
  see the page's keyboard shortcuts; let go to hide them.
- A pane that the page was only watching showed blank rows, or lost its first
  line, when the desktop's terminal was taller than the box on the page: the
  page scrolled to the padding below the last printed line. It now ends on the
  last line that printed something. This is what left both panes of an iPad's
  portrait split empty.
- The terminal's column count was measured with a different font stack than the
  one drawn, so a pane could be a few columns off on a system without
  JetBrains Mono.
- Search scrollback: the search button on a pane's header (and **Find in
  output** in the window menu) now looks through the pane's whole tmux history
  on the server, not just its last 2000 lines, and lists the newest 500 matches.
- The Copy bar for a long-press selection sits just above the rows you picked
  (below them when they start at the top) and follows as you drag or scroll,
  instead of jumping between the top and bottom of the pane.
- Opening a panel closes a preview and opening a preview closes the panel, so
  the two no longer overlap.
- Folding a project is one state everywhere: fold it on the web page and the
  desktop folds it too, and the other way round. It survives restarts with
  the desktop's layout. The fold arrow on a desktop project card works again:
  the project button around it was taking the click.
- A long agent turn no longer loses its beginning: tmux keeps 10000 lines per
  pane instead of 2000 (`history_limit` in `config.toml`), so scrolling back
  over omp or Claude reaches what the agent did, not only its closing summary.
  Panes opened before the change keep the old limit.
- On the desktop, a pane's right-click menu has **Attach selection**: it saves
  the selected text as a file in the paste stash and attaches it to the line
  like a pasted file. The app menu's **Recent attachments…** lists the stash
  (newest 50, with size and age); clicking a row attaches that file again.

### Review on the go

- Source control on the web page: a diff you open stays open (the 5-second
  refresh used to replace it, and to wipe a half-typed commit message), **Back**
  returns to the list at once, and the commit message survives refreshes.
- **Since last look.** Opening Source control leaves a mark (HEAD and time) in
  this browser; next time, the panel offers the commits after that mark and the
  uncommitted diff.
- A **PR line** (number, state, checks summary, link) for the current branch
  when `gh` is on `PATH` and knows one. `gh pr view` runs on a web thread and
  its answer is reused for 90 seconds; with no `gh` or no PR, nothing shows.

### Agent awareness on the web page

- The installed app's icon shows a badge with the number of windows waiting for
  an answer, and clears when none are. A push alert updates it even with the
  page closed.
- Every live terminal row, the stage header and the phone's header say how long
  the agent has been in its state: `busy 4m`, `waiting 2m`, `idle 1h`.
- A waiting window's answer card now offers `y`, `n`, `1`, `2` and `continue`
  (each sends the text and Enter), beside Enter, Reply and ^C.
- The lock-screen alert for a waiting agent has Yes and Continue buttons where
  the browser supports notification actions (Chrome on Android; iOS ignores
  them), answering without opening the page.

### Switch between TaiX hosts

Settings has a Hosts list kept in this browser: add a label and URL, remove, or tap one to switch. The command palette gets "Switch host: …" entries. Each host keeps its own tmux server, and pairing is per host.

### Preview a dev server from the phone

- The web page serves any port a pane's processes listen on at `/p/<port>/`,
  so a server bound to the desktop's loopback opens on a phone. It sits behind
  the pairing cookie and the passkey lock, and only ports the Ports panel lists
  are reachable. Responses stream, redirects stay inside the prefix, the
  pairing cookie is never forwarded, and a WebSocket upgrade is tunnelled.
- A previewed page is sandboxed (`Content-Security-Policy: sandbox` on every
  response, without `allow-same-origin`), so it cannot call TaiX with your
  pairing cookie. `/p/<port>/` redirects to a link only TaiX can make, which
  carries the page's own requests; the web socket and every write now also
  refuse a request whose `Origin` is not this host. The cost: the previewed app
  has no cookies or localStorage, and it must use relative URLs (Vite
  `base: './'`, webpack `publicPath: 'auto'`); a root-absolute `/assets/x.js`
  reaches TaiX, not the dev server.
- A Ports panel (globe in the rail, "Ports and preview" in the palette and the
  phone's More menu) lists each window's ports with **Preview** and **Open**.
  Preview shows the page beside the terminal, or full screen with a close
  button on a phone.

### See what every window is running, and where it serves

Every project and every terminal in the sidebar shows the TCP ports its
processes listen on (`:5173 +1`, as an accent pill) or how many processes it
runs. Hover one: a panel opens beside it with what is serving - port, program,
and whether it is reachable from this machine only or from the network - and
each window's process tree. Programs are named after what they run
(`node server.js`, `python3 http.server`), not `node-MainThread`. Click a port
and it opens in the project's browser window. Pane headers and a segment at
the end of the bottom bar, whose dot pulses while anything is serving, open
the same panel for one window and for the whole session.

The sidebar's ⋮ menus are visible again, dim until their row is hovered.

### The web page on an iPad

- An iPad in portrait gets the laptop's columns instead of the phone's single
  screen. Below 1100px wide (portrait, a Split View half) the project list is
  a drawer behind ☰ or the rail's Projects button, and panels float over the
  terminal's right edge instead of squeezing it. Phones are unchanged.
- The terminal is typed into directly, as on a laptop, so a Magic Keyboard's
  every key works. With the on-screen keyboard up, the key row (esc, tab,
  arrows, ctrl, symbols) sits above it; its sticky Ctrl applies to the next
  letter typed, so ^C is one tap and a key.
- Installed as an app, the header clears the status bar and the bottom bar the
  home indicator; launch screens for every current iPad, in both orientations.
- Finger-sized targets on a touch tablet; menus with nothing to anchor to open
  as a centred card; Split stacks its two panes in portrait.

### The web page is live, and installs as an app

- Opening a terminal from the page froze it until a reload: the page threw on
  every update while it waited for the new window. It now appears at once,
  opens on the stage, and is yours to type in.
- A stopped terminal starts when you tap it, as on the desktop; the stage says
  it is stopped and offers *Start* instead of an empty grid. Starting one from
  the page no longer moves the desktop's focus.
- Empty projects stay in the list, so they can be given a first terminal.
- Server-Sent Events and per-keystroke POSTs are replaced by one WebSocket
  (`/ws`) carrying snapshots down and commands up, reconnecting with backoff.
- PWA: real PNG and maskable icons, a manifest browsers can actually read (it
  was answered with the pairing page, so TaiX was never installable), a service
  worker that opens the app offline, and an install entry in the palette.
- On a phone, the key row is one row that scrolls sideways, with bigger
  keys (38px). A swipe along it scrolls instead of pressing the key under the
  finger. The `#` toggle is gone; Settings still hides symbols or controls.
- The gap between the line and the iPhone keyboard was the desktop's hidden
  keystroke field, which had no style and sat in the layout as a 38px box.
- Reloading on a phone showed an empty screen: the first snapshot set a view
  no rule displays. It now opens the window in the address, or home.
- Opening a terminal on a phone takes it: the grid becomes the phone's, so
  the pane fills the screen instead of showing the desktop's shape in its top
  half. The "Take control / Just reading" card is gone; if the desktop takes
  the terminal back, typing or reopening it on the phone takes it again.

### The iPhone, properly

- **Pair by camera.** Settings → Web shows a QR code of the pairing link. With
  *HTTPS over Tailscale* on, the link and the code are the `https://….ts.net`
  address, which is what an installable app, alerts and Face ID need. That
  switch now shows whether `tailscale serve` is really on, and in *Tailscale
  only* mode it proxies to the tailnet address the server is bound to instead
  of `127.0.0.1`, where nothing answered.
- **Lock-screen alerts.** Settings → *Notify me when an agent waits*: a Web Push
  arrives when a window starts waiting or fails, and tapping it opens that
  window. iOS delivers these only to the Home Screen app over HTTPS.
- **Face ID.** Settings → *Require Face ID on this device* registers a passkey;
  after five idle minutes or a desktop restart, the page asks for Face ID
  again, and the server refuses that device until it does.
- **Gestures on the terminal.** Swipe sideways to go to the next or previous
  window. Drag to scroll, with momentum, and inside full-screen programs that
  take the mouse (Claude Code, vim, htop) the drag now arrives as the wheel
  instead of doing nothing. Long-press a line to select rows, then Copy, Copy
  screen, or Paste into line.
- **Your own keys.** Long-press a key, or tap `+` at the end of the row, to add
  one that types text, runs a command, or presses a key. Long-press it to edit,
  move or delete; Settings lists them.
- **Sideways and with a keyboard.** A phone on its side shows two terminals of
  the project next to each other, each at its own size; turned back, the
  second one is given back to the desktop. With a hardware keyboard, ⌘1-9 and
  ⌘[ / ⌘] move between windows (Ctrl+Shift off Apple devices); ⌘T, ⌘. (^C)
  and ⌘↑ / ⌘↓ are bound too, though Safari may keep ⌘T and ⌘. for itself.
- Launch screens for the Home Screen app instead of a white flash, and haptics
  on iPhone through the iOS 18 switch, since it has no `navigator.vibrate`.
- A phone driving a pane the desktop is not focused on asked for the right grid
  but could lose the request and stay at the desktop's size; it is now checked
  against the pane and asked again.
- In the Home Screen app on iOS 26 the dock's buttons were squeezed against
  the home indicator. iOS 26 also paints nothing in the bottom status-bar's
  worth of an installed app, a WebKit regression no page can undo; that band
  now takes the dock's colour and the buttons sit on its edge, as low as the
  page can draw.
- Holding space on the iPhone keyboard (its trackpad) selected the whole page
  instead of moving the caret in the line; on touch screens only fields and
  file or diff text are selectable now.

## 0.2.0

### The browser is a window of the project

A browser window now sits in the grid beside the agent panes, opened from the
same menu as an agent or with `Ctrl+Shift+B`. It belongs to its project, keeps
its page when you switch away and back, and a project can have several. The old
browser side panel is gone.

### Agents can drive those windows

`taix mcp` gained ten browser tools, named after Playwright MCP's because that
is what models already know: `browser_tabs`, `browser_navigate`,
`browser_snapshot`, `browser_click`, `browser_type`, `browser_press_key`,
`browser_evaluate`, `browser_wait_for`, `browser_console`,
`browser_screenshot`.

They drive the window you are looking at, so you watch the agent work instead
of reading about it. Snapshots are an accessibility outline with `[ref=e7]`
handles; every other tool answers in one line, so a ten-step flow costs one
snapshot rather than ten. `browser_tabs {"action":"new","headless":true}` opens
a window with no card on screen for the checks you do not want to watch.

### Tab groups

Drop one pane's header on another pane's header to put both windows in one tab
group. A tab carries its window's state, so a background tab that needs input
says so. Drag it out, double-click it, or press the split button to give it
half the pane back; middle-click closes it. Underneath they are still separate
tmux windows, still visible to the TUI and the phone.

### Settings has an MCP section

The `claude mcp add` and `codex mcp add` lines, the config-file JSON, and the
endpoint and key the browser tools use — each with a copy button.

### Fixed

- Panes went black for a fraction of a second whenever a neighbouring window
  printed anything.
- A pane could come back permanently blank after switching projects, until you
  scrolled it.
- Panes could vanish when a tab group and a third window shared a project.
- The desktop reloaded its own writes, which fought with the layout.

## 0.1.0

First release. TaiX runs your AI coding agents as tmux windows and gives you
three ways to watch them: a GTK desktop window, a terminal UI, and a web page
you open on your phone.
