# Changelog

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
