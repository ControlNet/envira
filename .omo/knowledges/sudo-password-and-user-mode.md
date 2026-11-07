# sudo passwords, user mode and askpass (2026-11-07)

## The bug this fixed (there since M1)

Steps run in a session of their own (`setsid` in `runner::RealRunner::command`, so cancelling
kills the session and installers cannot block on the TUI's terminal). sudo keys its ticket by
terminal and session (`timestamp_type=tty`, the default on Ubuntu, Debian and macOS), or by
parent process when there is no terminal. So a password given to `sudo -v` in envira's session
(the old CLI/TUI prompt), or a ticket the user's own `sudo` left in the terminal, is **not seen**
by a step's `sudo -n`. Every sudo step failed with "a password is required" for anyone whose
sudo asks for a password. CI never showed it: its containers have NOPASSWD.

Reproduction (`envira-it-ubuntu`, user in group sudo with a password, sudo 1.9.15): from one
parent process, `sudo -S -v` with the password succeeds, `sudo -n true` in the same session
succeeds, and `sudo -n true` with `start_new_session=True` fails.

## How it works now

- `{sudo}` expands to `sudo -A` (it was `sudo -n`), and so does the as-root check in
  `runner::capture_command`. Steps get `SUDO_ASKPASS` while a run holds the password. Otherwise
  the runner removes `SUDO_ASKPASS`, so `sudo -A` fails at once like `-n` did and never opens a
  desktop askpass the user had set.
- `askpass::Askpass::start(password)` does the following:
  - makes a 0700 temp dir holding a symlink `envira-askpass` to envira itself and a socket;
  - `main` checks `argv[0]`'s basename before clap and answers as the helper;
  - only processes of the same uid are answered, and on Linux only descendants of this envira
    (SO_PEERCRED plus `/proc/<pid>/stat` parents).

  In the container, a process of the same user started outside envira got 0 bytes.
- "Can steps use sudo without a password?" (`facts::sudo_runs_without_password`) runs
  `sudo -n true` in a new session, so a terminal ticket does not count.
- `envira plan --script` and the TUI's saved script print plain `sudo`; `sudo -A` is only for
  envira's own runs.

## User mode

- `Facts::without_sudo()` gives privilege None and `user_mode = true`, and the plan is made
  again with it:
  - items with a user route take it;
  - the rest are skipped with "needs sudo";
  - portable builds are not copied to `/usr/local/bin`.
- An update of a tool installed system-wide becomes a user-scope install of that tool
  (`reason`: "user copy: its system install needs sudo"). User bin dirs come first on PATH.
- When the prompt appears:

  | Situation | What happens |
  |---|---|
  | sudo wants a password, interactive | The prompt appears (CLI line, or TUI dialog over the plan): password field, **Use user mode**, Back/Cancel |
  | Passwordless sudo | Nothing is asked |
  | `-y` or no terminal | User mode, with a note |
  | `--scope system` | The prompt without the user-mode button; without a terminal it fails |
  | `--scope user` | User mode from the start (session facts) |

- The password goes to `sudo -S -p "" -v` on stdin with `LC_ALL=C`. "not in the sudoers",
  "not allowed to" or "may not run sudo" mean Denied; anything else means a wrong password.
  `sudo_prompt::Secret` zeroes its whole buffer on drop.
- Sudo rights that come from a sudoers rule envira cannot see without a password: the user
  chose option A (no guessing). Such users pass `--scope system` to get the prompt.

## Testing it

There is no catalog item with an offline sudo step. Use a test-only overlay in
`~/.config/envira/catalog.d/` inside the container:
- an item with a sudo route and a user route (each `touch`es a file);
- an item that is sudo only;
- a slow sudo item, for probing the socket while it runs.

Drive envira with a python pty:
- set the window size with `TIOCSWINSZ`, otherwise a 0x0 pty draws nothing;
- for the TUI, step on timers and check the files, not the screen text: ratatui redraws
  only the cells that changed, so phrases rarely appear whole in the raw output.

Container setup: `rm /etc/sudoers.d/*; usermod -aG sudo user; chpasswd`.
