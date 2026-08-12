# ratatui notes — what reporch actually needs

Distilled from **ratatui 0.29.0**, read from the vendored crate source at
`~/.cargo/registry/src/*/ratatui-0.29.0/`, cross-checked against <https://ratatui.rs>. Written
before any TUI code, so the architecture is shaped by the framework rather than fighting it.

The version matters: several of these APIs were renamed or added recently, and writing 0.24-era
ratatui from memory produces code that does not compile against 0.29.

---

## 1. Immediate mode is the whole design

> *"Immediate mode rendering is a UI paradigm where the UI is recreated every frame."*

There are no persistent widget objects. Every frame, reporch constructs widgets from application
state, draws them, and throws them away. The consequences that matter:

- **State lives outside widgets, in reporch's own structs.** A `List` does not remember which row
  is selected — a `ListState` that reporch owns does.
- **Conditional UI is just an `if`.** No mount/unmount, no visibility flags.
- **The event loop is reporch's problem.** ratatui draws; `crossterm` reads input; neither runs a
  loop for you. If the draw thread blocks, the UI freezes.

This is the single most important thing to internalise: **do not build a widget tree.** Build a
state struct, and write a `draw(frame, &mut state)` function that reads it.

The shape reporch uses everywhere:

```rust
loop {
    terminal.draw(|frame| ui::draw(frame, &mut app))?;   // pure function of app state
    if !event::poll(tick)? { app.on_tick(); continue; }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
        Event::Resize(..) => {}                            // next draw handles it
        _ => {}
    }
    if app.should_quit { break; }
}
```

`KeyEventKind::Press` is not optional — on Windows and on terminals with the Kitty keyboard
protocol, crossterm delivers `Press` *and* `Release`, and without the guard every keystroke acts
twice.

---

## 2. Terminal setup and restoration

0.29 provides the whole lifecycle, and reporch should use it rather than hand-rolling
`enable_raw_mode` / `EnterAlternateScreen`:

```rust
let mut terminal = ratatui::init();   // raw mode + alternate screen + panic hook
let result = run(&mut terminal);
ratatui::restore();                   // disable raw mode, leave alternate screen
result
```

- `ratatui::init() -> DefaultTerminal` where `DefaultTerminal = Terminal<CrosstermBackend<Stdout>>`.
- **`init()` installs a panic hook that restores the terminal before panicking.** Verified in the
  source: `set_panic_hook()` takes the existing hook, and wraps it so `restore()` runs first, then
  the original hook prints the message.
- `try_init()` / `try_restore()` return `io::Result` if you need to handle failure.

Two traps the source documents explicitly:

1. **Install your own panic hooks *before* calling `init()`.** ratatui chains onto whatever hook
   exists at the moment `init()` runs. A hook installed afterwards runs before the restore and will
   print into the alternate screen, where the user never sees it.
2. `restore()` disables raw mode *first*, then leaves the alternate screen — that order matters
   because raw mode has the wider side effects.

reporch relies on this hook for the M1 "terminal is restored on panic" requirement, and verifies it
with a deliberate panic behind a hidden `--panic-test` flag rather than trusting the docs.

**This is also why `tracing` must never write to stdout or stderr** (CLAUDE.md says so). stdout is
the alternate screen; a log line drawn into it corrupts the frame. reporch logs to
`~/.reporch/logs/reporch.log` only.

---

## 3. Layout

`Layout` splits a `Rect` into child `Rect`s. Nothing is nested implicitly — you split, then split
a result again.

```rust
use ratatui::layout::{Constraint, Layout};

// Preferred in 0.29: const-generic destructuring, no indexing, no unwrap.
let [header, body, footer] = Layout::vertical([
    Constraint::Length(3),
    Constraint::Min(0),
    Constraint::Length(1),
]).areas(frame.area());

let [list, detail] = Layout::horizontal([
    Constraint::Percentage(40),
    Constraint::Percentage(60),
]).areas(body);
```

- `Layout::vertical(..)` / `Layout::horizontal(..)` are the terse constructors.
- **`.areas::<N>()` returns `[Rect; N]`** and panics only on a genuine programmer error (count
  mismatch). Prefer it over `.split()`, which returns a slice and invites indexing.
- `frame.area()` — **not** `frame.size()`, which is `#[deprecated]` in 0.29.

Constraint variants: `Length(u16)`, `Min(u16)`, `Max(u16)`, `Percentage(u16)`, `Ratio(u32,u32)`,
`Fill(u16)`. `Fill` distributes leftover space proportionally and is what the run screen's repo
table wants for its widest column.

---

## 4. Widget vs StatefulWidget, and where selection lives

```rust
pub trait Widget          { fn render(self, area: Rect, buf: &mut Buffer); }
pub trait StatefulWidget  { type State; fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State); }
```

`StatefulWidget` is how ratatui keeps scroll offset and selection **without owning them**. reporch
stores the `ListState` / `TableState` in its own app struct and passes `&mut` in each frame:

```rust
frame.render_stateful_widget(list, area, &mut self.menu_state);
```

`ListState` (0.29) gives the navigation helpers directly, so reporch writes no index arithmetic:

```rust
state.select(Some(0));
state.select_next();      // saturates at the end
state.select_previous();  // saturates at 0
state.select_first();
state.select_last();
```

These saturate rather than wrap. The §5.1 menu wants wrapping, so reporch wraps explicitly in its
own key handler and uses `select(Some(i))` — a deliberate deviation from the helpers, not an
oversight.

---

## 5. What reporch renders, mapped to widgets

| Screen (§) | Widget |
| :--- | :--- |
| §5.1 top-level menu, §5.5 project menu | `List` + `ListState`, custom `›` prefix via `ListState` highlight symbol |
| §5.3 repo multi-select | `List` + `ListState`; the `[x]` checkbox is drawn into the `ListItem` text, since selection-for-navigation and selection-for-inclusion are different state |
| §5.3 fuzzy filter | `nucleo-matcher` over the candidate list, re-run on each keystroke; the widget just renders the filtered vector |
| §5.6 brief editor | `tui-textarea` (multi-line, its own state), `$EDITOR` handoff for anything long |
| §5.8 run screen | `Table` + `TableState`; `Constraint::Fill` on the LAST column |
| §12.1 consent screen | `Paragraph` in a `Block`, `.wrap(Wrap { trim: false })` to preserve the verbatim layout |
| mailbox / brief viewer | `Paragraph` with a manual scroll offset in app state |

---

## 6. Traps worth writing down before hitting them

- **`frame.size()` is deprecated** → `frame.area()`.
- **Key repeat double-firing** → always guard on `KeyEventKind::Press`.
- **`Layout` is cached** by an internal LRU keyed on the layout+area. Building a `Layout` per frame
  is cheap and idiomatic; do not hoist it into state for "performance".
- **Unicode width** — repo names and brief subjects can contain wide characters. Truncate with a
  width-aware helper, never `&s[..n]`, which panics on a char boundary.
- **Draw is not throttled.** Without a `poll` timeout the loop spins at 100% CPU. reporch polls on
  a tick (250ms) and only redraws when something changed or the tick fires.
- **Never `println!`** anywhere in the binary once the TUI is up — including in a `Drop`.
- **The alternate screen hides panic output**, which is exactly why the panic hook exists and why
  M1 tests it for real.

---

## 7. Consequence for reporch's architecture

The immediate-mode model lines up with §13.3's actor loop rather than fighting it. Session events
arrive on a channel, the supervisor folds them into `TaskState`, and the render task draws
`TaskState`. Because widgets hold nothing, there is no synchronisation problem between "the state"
and "the UI" — there is only the state, drawn 4×/second.

That is why `reporch-core`'s `(TaskState, Event) -> (TaskState, Vec<Action>)` signature is
compatible with the TUI at all: the UI is a pure projection of a value the core already owns.
