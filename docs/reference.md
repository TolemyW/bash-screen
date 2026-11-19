# bash-screen reference

Material to look up. For a first run, see the [README](../README.md).

## How it works

- The agent calls the `bash-screen` CLI. Each call is one SSH round trip to a
  helper on the remote host; one SSH connection is reused across calls.
- The remote host runs an isolated GUI session: an Xvnc virtual display, the
  openbox window manager, and a private session bus. Nothing listens on a
  network port.
- The helper reads the UI through
  [agent-desktop](https://github.com/vercel-labs/agent-desktop) (the
  accessibility tree) and falls back to screenshots and coordinates for
  anything the tree does not cover.

## Install

`herdr plugin install` downloads the binary for your system from the GitHub
release, checks it against the release's checksums, and links `bash-screen`
into `~/.local/bin`. The link is refreshed at every herdr start, so it follows
plugin updates. It never replaces a file that is not its own link.

Where there is no prebuilt binary the install builds from source instead,
which needs Rust. Set `BASH_SCREEN_BUILD_FROM_SOURCE=1` to force that.

## Remote host

Tested on Ubuntu 22.04, 24.04 and 26.04 and Debian 12 and 13. Older releases
do not work, because agent-desktop needs glibc 2.35.

The host must already have these packages:

- `dbus`, `at-spi2-core`, `x11-xkb-utils`
- Python 3 with Pillow
- on Ubuntu 22.04 also `xkb-data`, which any host with a desktop has
- optionally `python3-gi` and `gir1.2-atspi-2.0`, for [reading text](#reading-text)

`init` checks for them and prints the `apt-get install` command an
administrator should run if any are missing. Everything else (Xvnc, openbox,
agent-desktop) is unpacked into the remote directory without root.

`bash-screen init` runs three steps that are also available on their own:
`config`, `setup` and `start`. Finished steps are skipped when it runs again.
`--remote-dir` and `--geometry` change the defaults.

## Observations, refs and names

`observe` lists the elements of the current window that can be acted on, each
with a ref such as `e12`. `--all` adds named elements that cannot, such as
headings.

- A ref stays with its element from one observation to the next. Whether an
  element is still the same one is judged by its role, its name and its place
  among the elements that share both.
- A ref is never given to a second element. One whose element has gone fails
  with `e12 is not in the window any more`.
- Elements with the same role and name, such as a `Delete` button in every
  row, are told apart only by their order. When one goes, each one after it
  takes over the ref of the one before, and the last ref is reported gone.
- An element whose name changes, such as a button going from `Save` to
  `Saving…`, counts as one gone and one new.

After an action, a `wait` or a `do`, the reply says what changed since the
observation before:

```
window: Orders — Mozilla Firefox (firefox-bin)
3 changed (+ new, ~ different, - gone); the other 28 elements and their refs are as before
+ [e31] button "Undo"
~ [e28] textfield "Name" = "Ada"
- [e27] button "Delete"
```

- The whole list is printed instead when the current window is another one,
  when the observation before was made with `--all`, or when more than half
  of the elements (and more than 12) changed.
- `observe` always prints the whole list.
- With `--json`, `text` is always the whole list and `brief`, when present,
  is the report of changes.

`click`, `type` and `scroll` take `--name <text>` in place of a ref.

- The window is read at that moment, so no observation is needed first, and
  in a `do` a name can aim at an element that an earlier step brought up.
- An element matches when its name is the text, whatever the case. If none
  is, elements whose name contains the text match.
- It fails when no element matches, and when several do; the error lists
  them.

## Reading text

`bash-screen text` prints what the current window says, without refs. In a
browser it prints only the page, not the toolbars.

- A paragraph is one line, with the text of its links in place.
- A table row is one line, with ` | ` between cells.
- An input shows its content in `[ ]`; a password input shows `*` for each
  character.
- Parts that are not showing, such as other tabs, are left out. Text that is
  only scrolled out of view is included.

This reads the accessibility bus directly and needs two more packages on the
host, `python3-gi` and `gir1.2-atspi-2.0`. `init` works without them and
names them as optional when they are absent. Without them `text` prints the names of the elements only, says so, and
leaves out running text such as paragraphs; `wait --text` and `wait --gone`
then do not see that text either.

## Waiting and batches

`bash-screen wait` returns an observation once its conditions hold:

| Option | Holds when |
|---|---|
| `--text <text>` | The name or value of some element in the current window, or a line of its text, contains the text |
| `--gone <text>` | No element contains the text |
| `--title <text>` | The title of the current window contains the text |
| `--stable` | Two screenshots taken half a second apart are identical |
| `--timeout <seconds>` | Not a condition: how long to wait, 15 by default and 300 at most |

- Several conditions can be given; all must hold at the same check.
- Without a condition, `wait` behaves as `--stable`.
- Text is compared without regard to case, and as part of a longer string:
  `--gone Save` keeps waiting while an element reads `Saved`.
- The window is checked every half second. A check of a large window can
  take longer than that by itself.
- On a timeout the command fails and says which conditions were not met and
  what the window is titled.
- `--text` and `--gone` read the window's elements, so they are of no use
  with an app started with `launch --no-a11y`. `--stable` is. A page that never
  stops moving, such as one with a blinking caret or a clock, never becomes
  stable.

`bash-screen do` sends several steps as one request and observes once at the
end:

```sh
bash-screen do click e7 then type "Ada" then key tab return then wait --text Saved
```

- The steps are `click`, `type`, `key`, `scroll`, `drag` and `wait`, with
  their usual arguments. Other commands are rejected before anything is sent.
- Steps are separated by the word `then` on its own. The single word `then`
  can therefore not be typed in a batch; text that only contains it can.
- Nothing is observed between the steps, so refs mean what they meant in the
  observation made before the batch, for every step. A ref to an element
  that has since disappeared fails.
- A failing step ends the batch. The error names the step and says how many
  were done before it; nothing is undone.
- With `--json`, the reply has `steps`, the number of steps that ran. The
  reply of `wait` has `waited`, in seconds.

## Input from a viewer

`bash-screen input` is for a program that shows the screen to a person, such
as the [Herdr app mod](../herdr-app/RECIPE.md). It is not listed by `--help`
and is not meant for an agent: it injects the actions and returns at once,
without the observation that `click`, `type` and `key` print.

```sh
bash-screen input '[{"kind":"click","x":640,"y":400},{"kind":"text","text":"hi"},{"kind":"key","key":"return"}]'
```

The argument is a JSON list. Each action has a `kind`:

| `kind` | Other fields | Effect |
|---|---|---|
| `click` | `x`, `y`, optional `button` (`left` by default) | A click at that screen position |
| `wheel` | `x`, `y`, `dy` | Turns the wheel at that position; negative is down |
| `text` | `text` | Pastes the text at the keyboard focus, replacing the remote clipboard |
| `key` | `key` | A key or combination, as for `bash-screen key` |

- The actions run in order in one round trip. The first one that fails ends
  the call; the ones before it stay done.
- The reply is the number of actions done, as `done` with `--json`.
- Each action appears in the action log marked `you:`.
- It does not open the preview pane.

A viewer takes its pictures with `bash-screen look --viewer`, which differs
from a plain `look` in three ways:

- With `--json`, the reply also has `events`, the last 60 entries of the
  action log (each with `seq`, `ok` and `what`), and the `host` and `session`
  the picture is of.
- It records that a viewer of that session is open. For ten seconds after,
  commands in the Herdr app do not open a pane for that session.
- It does not open the preview pane.

With `BASH_SCREEN_FOLLOW=<key>` in its environment, a viewer that a command
opened beside a terminal acts on the session that terminal last used, whatever
`--host` and `--session` say. The key is given to the mod as the prop
`follow`.

## Preview

The preview pane follows the session its calling pane last used. In the Herdr
desktop app it needs no plugin, only `bash-screen`, `herdr-app` and `hc` on
the `PATH`. `bash-screen preview --host <host> --session <name>` chooses what
a preview of your own shows.

In the Herdr desktop app the pane is the [mod](../herdr-app/RECIPE.md) when
version 0.5.2 or later of it is installed, and the preview in a terminal
otherwise.

- No pane is opened while a mod pane shows the session, whether a command
  opened it or you did.
- A terminal preview that is already open beside the calling pane is kept.
  Close it to get the mod pane at the next command.
- The keyboard stays with the pane that had it.

It draws a real image when the terminal supports the Kitty graphics protocol,
and block characters otherwise. Its text goes wherever the pane has spare
room: a column beside the picture, or lines below it.

Besides the keys in the preview pane, the resolution can be changed:

- from anywhere, with `bash-screen res up`, `res down`, `res 960` or
  `res full`;
- in herdr, by binding the plugin actions `bash-screen.sharper` and
  `bash-screen.coarser` to keys.

While you are in control of the screen:

- The picture is sent at 960 pixels or more.
- Text is entered by pasting, which replaces the remote clipboard.
- Dragging with the mouse is not supported, and keys that herdr or the
  terminal keep for themselves never reach the pane.
- Your actions appear in the action log marked `you:`. Nothing stops you and
  the agent from acting at the same time.

## Configuration

`bash-screen config` prints the file in use, normally
`~/.config/bash-screen/config.toml`.

| Key | Default | Meaning |
|---|---|---|
| `host` | | The default host: an SSH host as you would pass it to `ssh`, or `local` |
| `remote_dir` | `~/.bash-screen` | Directory on the default host holding the tools |
| `geometry` | `1280x800` | Size of the virtual screen, fixed per session |
| `preview_direction` | `auto` | Where the preview pane opens: `auto` (right of a wide pane, above a narrow one), `right`, `down` or `up` |
| `preview_blocks` | `auto` | How the preview draws: `auto`, `image`, `sextant`, `quad` or `half` |
| `preview_max_px` | `640` | Longest edge of the picture sent to the preview; `0` sends the full screen |

Another host can have its own `remote_dir` and `geometry`:

```toml
[hosts.other-host]
remote_dir = "~/tools/bash-screen"
```

## Environment variables

| Variable | Effect |
|---|---|
| `BASH_SCREEN_CONFIG` | Use this config file |
| `BASH_SCREEN_HOST` | Host to act on, as `--host` |
| `BASH_SCREEN_SESSION` | Session to act on, as `--session` |
| `BASH_SCREEN_NO_PREVIEW` | Do not open the preview pane |
| `BASH_SCREEN_BUILD_FROM_SOURCE` | Build from source at install time instead of downloading a binary |
| `BASH_SCREEN_DEBUG` | Make the preview log the timing of each redraw to `~/.local/state/bash-screen/preview-timing.log` |

## Known limits

- Electron and Chromium apps can crash on a key press after their elements
  have been read. Start them with `launch --no-a11y`. Only one Electron app
  has been tried, and Chrome itself has not.
- When a dialog covers a page, `observe` still lists the covered elements,
  ahead of the dialog's own.
- A dropped connection is not resumed, and a repeated request is not
  recognised as a repeat.
- A session shares the remote user's home directory; only the display and the
  session bus are isolated. Each session does get its own Firefox profile.
