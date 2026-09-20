# Rendered with VHS: https://github.com/charmbracelet/vhs
#   cargo build --release && vhs demo.tape
#
# Everything runs against a throwaway config, data dir and tmux socket, so
# recording this does not touch the projects you actually have open.

Output assets/demo.gif

Require tmux

Set Shell "bash"
Set FontSize 20
Set Width 1200
Set Height 700
Set Padding 20
Set Margin 30
Set MarginFill "#1e1b2e"
Set BorderRadius 10
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

# --- setup, not recorded ---
Hide
Type `export PATH="$PWD/target/release:$PATH"`
Enter
Type `export XDG_CONFIG_HOME=$(mktemp -d) XDG_DATA_HOME=$(mktemp -d)`
Enter
Type `mkdir -p "$XDG_CONFIG_HOME/taix" && printf 'tmux_socket = "taixdemo"\ntmux_session = "demo"\nweb.enabled = false\n' > "$XDG_CONFIG_HOME/taix/config.toml"`
Enter
Type `mkdir -p /tmp/taix-demo/api /tmp/taix-demo/web && clear`
Enter
Show

# --- register two projects ---
Type "taix add /tmp/taix-demo/api"
Enter
Sleep 700ms
Type "taix add /tmp/taix-demo/web"
Enter
Sleep 700ms
Type "taix ls"
Enter
Sleep 1500ms

# --- which agent CLIs are on this machine ---
Type "taix harnesses"
Enter
Sleep 2500ms
Type "clear"
Enter

# --- the TUI: t opens a terminal window, Enter types into it ---
Type "taix"
Enter
Sleep 2500ms
Type "t"
Sleep 2s
Enter
Sleep 600ms
Type "echo the pane is live"
Enter
Sleep 1500ms
Ctrl+]
Sleep 800ms
Type "q"
Sleep 1500ms

# --- teardown, not recorded ---
Hide
Type `tmux -L taixdemo kill-server 2>/dev/null; rm -rf /tmp/taix-demo`
Enter
Show
Sleep 2s
