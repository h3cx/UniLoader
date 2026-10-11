# UniLoader setup (Linux)

UniLoader imports component ZIPs or folders into global KiCad libraries or a
portable project. It watches downloads during your graphical login session and
asks where to import each component. It creates missing libraries and registers
them in the selected scope automatically.

## Requirements and installation

Install Rust 1.95 or newer and your distribution's
C build tools and `pkg-config`. Desktop dialogs use Rust
[egui/eframe](https://docs.rs/eframe/latest/eframe/) with the OpenGL renderer and
both Wayland and X11 support. File browsing uses
[rfd's XDG desktop portal backend](https://docs.rs/rfd/latest/rfd/#linux--bsd-backends).
Install `xdg-desktop-portal` and the backend appropriate for your desktop
(for example, KDE or GTK). A working OpenGL driver, display session and session
D-Bus are required. `notify-send` is optional for desktop notifications.

The UI is compiled into `uniloader`; there are no companion scripts to install.
The watcher starts another instance of the same executable internally for each
dialog, keeping the window event loop on its own main thread.

On Debian/Ubuntu, a starting set of build packages is `build-essential pkg-config
libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev
libwayland-dev`; other distributions provide corresponding development packages.
For optional notifications install `libnotify-bin` on Debian/Ubuntu or `libnotify`
on Arch Linux. The portal backend can bring its own toolkit dependencies; UniLoader
does not link a GTK UI. See the [upstream eframe build notes](https://github.com/emilk/eframe_template/#testing-locally)
if compilation reports a missing system library.

Build from the repository directory:

```sh
cargo build --release --locked
install -Dm755 target/release/uniloader ~/.local/bin/uniloader
~/.local/bin/uniloader --help
```

Add `~/.local/bin` to your shell's `PATH` to use the shorter `uniloader` command.
Do not run the importer as root.

The importer targets KiCad's modern `.kicad_sym`, `.kicad_mod` and `.kicad_pro`
formats. It preserves source format versions; the installed KiCad must support
those versions. Legacy `.lib` symbol conversion is not included. Verify your
actual component in KiCad before relying on a library for a board design.

This release discovers possible open projects by inspecting Linux KiCad process
arguments and working directories. These are **suggestions**, not an authoritative
active-window selection: startup arguments can be stale, editor navigation can
change documents, and some launchers hide the file argument. IPC discovery is a
follow-up; no IPC configuration is required for this release. Always check the
project path in the destination dialog, or use Browse / `--project`.

## Configuration

The default configuration is `${XDG_CONFIG_HOME:-~/.config}/uniloader/config.toml`.
A missing configuration is generated with defaults on first launch. You can
select another file with `--config /absolute/path/config.toml`. Existing
`[libraries].path` and `[options]` settings remain supported. `~` expands in paths;
relative configuration paths resolve from the process working directory, so use
absolute or home-relative paths for a service.

A minimal setup is:

```toml
[libraries]
path = "~/KiCad/libraries"
# Select your installed KiCad version for global library registration:
kicad_config = "~/.config/kicad/9.0"
```

Use your actual KiCad version directory. When a single installed configuration
can be identified, UniLoader can discover it. If none or several are found,
set `kicad_config` explicitly before importing globally. Project imports create
project-local tables and do not require a global table.

The complete example is in `config/config_full.toml`. Available settings are:

```toml
[libraries]
path = "~/KiCad/libraries"
kicad_config = "~/.config/kicad/9.0"
search_paths = []
model_search_paths = ["~/KiCad/models"]

# Optional KiCad variables not already supplied by your environment:
[libraries.variables]
# KICAD9_3DMODEL_DIR = "/usr/share/kicad/3dmodels"

[options]
symbols = true
footprints = true
models = true
preserve_source = true

[watch]
directories = ["~/Downloads"]
settle_seconds = 2
retry_seconds = 60

[destination]
policy = "ask" # ask, global, project
# project = "/absolute/path/Board/Board.kicad_pro"
portable = true

[matching]
enabled = true
exact = "reuse" # reuse, ask
ambiguous = "ask" # ask, separate, skip
# Pad-position comparison tolerance, in millimetres:
tolerance = 0.15

[ui]
enabled = true
```

`destination.policy = "project"` requires a fixed `project` path. A fixed target
can be used when KiCad is closed. The default `ask` offers detected projects,
Global and Browse. Remembering a choice applies to the running session; restart
UniLoader to clear it. Footprint reuse mappings are persisted by content; to clear
those mappings, stop UniLoader and remove only the content-mapping JSON files in its data
directory’s `decisions/` subdirectory, then restart. Explicit fixed configuration persists until edited.

Matching searches effective registered libraries and configured `search_paths`.
Do not add an enormous recursive root such as your entire home directory.
Missing/unreadable search locations are reported rather than silently treated
as proof that no duplicate exists. Exact footprint reuse is conservative;
similar footprints or conflicting names prompt for comparison. The preview
shows both footprints at the same scale, numbered pads, bounds and differences.
It approximates some shapes and omits text, drill holes and 3D geometry; the
window reports limitations. Inspect source files in KiCad when necessary.
`ambiguous = "separate"` or `"skip"` permits an explicit noninteractive policy;
the default keeps unanswered choices pending.

## Foreground smoke test

Start with an expendable test project and a known downloaded component:

```sh
uniloader discover
uniloader import /path/to/component.zip --project /path/to/Test/Test.kicad_pro --dry-run
uniloader import /path/to/component.zip --project /path/to/Test/Test.kicad_pro
uniloader import /path/to/component.zip --global --dry-run
uniloader watch
```

A manual import also accepts an extracted component folder. No-argument startup
runs the watcher. Dry run reports choices and intended library changes without
writing libraries; unresolved decisions may need an interactive import.
Use `--noninteractive` to avoid opening dialogs. A pending choice is not consent.

Project imports create `libraries/UniLoader.kicad_sym`,
`libraries/UniLoader.pretty/`, and model storage as needed, and merge registration
entries into `sym-lib-table` and `fp-lib-table` beside the `.kicad_pro` file.
Project paths use `${KIPRJMOD}`. Portable mode copies selected external footprints
and available referenced models into the project. Global imports use the
configured library root and register entries in the selected KiCad configuration.
Existing unrelated table entries are preserved. Existing schematic and PCB
instances are not automatically replaced by a library import.

Open the imported symbol, footprint and model in KiCad. Refresh/reopen the
library browser or editor if KiCad has cached the table/library. For an existing
placed component, use KiCad's normal update-from-library operation when you want
it updated. Copy the test project to another directory and verify its imported
assets again to check portability.

The watcher handles component ZIP downloads (including common SamacSys,
UltraLibrarian and SnapMagic layouts); use a manual folder import for extracted
assets. Source archives are preserved by default. Incomplete or invalid ZIPs
are retried for the configured period, then remain recoverable rather than
stopping subsequent downloads.

## 3D models and broken paths

STEP (`.step`/`.stp`) and VRML (`.wrl`) models are copied when referenced by an
imported footprint. Resolution tries existing paths, bundle files and configured
model search roots. Windows/provider absolute paths and obsolete extraction
roots can be repaired through unique relative suffixes or filenames. Different
matching model files require a selection rather than an arbitrary choice.
Model transforms remain attached to the reference. Project references point to
`${KIPRJMOD}/libraries/models/...`; global references use absolute storage paths.

Missing models leave the usable component import available with warnings and
retain the original reference. Such an import is not fully portable. Adding the
missing model to a search root and deliberately reimporting with `--force` can
repair the dependency. Disabling `options.models` may also leave external paths.
UniLoader does not download models or convert CAD formats.

## Pending imports and recovery

```sh
uniloader pending
uniloader pending --all # include completed/skipped imports
uniloader retry IMPORT_ID
uniloader retry IMPORT_ID --dry-run --noninteractive
```

Closing a dialog or choosing Later leaves a pending import; it does not choose
Global, overwrite a file, or guess a model. Retry after fixing the configuration,
restoring the display environment or obtaining a missing asset. Other downloads
can continue through the queue. `--force` deliberately reimports a completed
source when you need to revisit it.

Import state/history lives beneath `${XDG_DATA_HOME:-~/.local/share}/uniloader`;
temporary extraction uses `${XDG_CACHE_HOME:-~/.cache}/uniloader`.
Changed files have recovery backups recorded by the import result/history.
Stop the watcher before manually restoring a backup, compare the current file
with the backup, and copy back only the intended version. A backup may predate
later manual edits; do not blindly restore every file. Restart KiCad or refresh
its libraries afterwards. Automatic undo is not included.

## Run continuously with a systemd user unit

First verify the foreground test above, then install the shipped service:

```sh
install -Dm644 systemd/uniloader.service ~/.config/systemd/user/uniloader.service
systemctl --user daemon-reload
systemctl --user enable --now uniloader.service
systemctl --user status uniloader.service
journalctl --user -u uniloader.service -f
```

The service uses `%h/.local/bin/uniloader watch` and the default configuration.
It starts with `graphical-session.target`, stops with that target, and restarts
after unexpected failure. “Always running” means while logged into your desktop.
To use a nondefault config, run `systemctl --user edit uniloader.service` and add:

```ini
[Service]
ExecStart=
ExecStart=%h/.local/bin/uniloader --config %h/.config/uniloader/work.toml watch
```

Then reload/restart:

```sh
systemctl --user daemon-reload
systemctl --user restart uniloader.service
```

### Display/session troubleshooting

If imports remain pending and logs say dialogs are unavailable, try a manual
interactive import from your desktop terminal and check the reported display or
OpenGL error. Check the service manager environment with
`systemctl --user show-environment`; it needs the real session's display variables
and session D-Bus. File browsing also needs a running desktop portal and a backend
that supports file selection; check `journalctl --user -u xdg-desktop-portal` if
Browse fails. Never invent a `DISPLAY=:0` or copy another user's Xauthority.

Some window managers/compositors do not activate `graphical-session.target` or
import display variables. Put this shell hook in your compositor's session-start
configuration, after the graphical session exists:

```sh
#!/bin/sh
for variable in DISPLAY WAYLAND_DISPLAY XAUTHORITY XDG_RUNTIME_DIR XDG_CURRENT_DESKTOP XDG_SESSION_TYPE DBUS_SESSION_BUS_ADDRESS; do
    if printenv "$variable" >/dev/null 2>&1; then
        systemctl --user import-environment "$variable"
    fi
done
systemctl --user start uniloader.service
```

If the service was already running with a stale environment, restart it after
importing variables, then retry pending imports. For compositors without target
lifecycle integration, add `systemctl --user stop uniloader.service` to your
session-exit hook. Enabling a unit alone cannot supply a missing GUI session.
The initial environment comes from the session startup hook, not a hardcoded
service `Environment=` line.

Lingering (`loginctl enable-linger`) does not create a graphical session and is
not needed here. Operation before login or after logout requires a configured
noninteractive destination and explicit ambiguity policy; unanswered decisions
will remain pending.

### Update, stop and remove

After updating the source:

```sh
cargo build --release --locked
systemctl --user stop uniloader.service
install -Dm755 target/release/uniloader ~/.local/bin/uniloader
systemctl --user start uniloader.service
```

Keep your existing config; compare it with the updated complete example for new
settings. A service restart reloads configuration.

To disable automatic startup:

```sh
systemctl --user disable --now uniloader.service
```

To remove the service and executable:

```sh
rm ~/.config/systemd/user/uniloader.service
systemctl --user daemon-reload
rm ~/.local/bin/uniloader
```

Remove any compositor hooks or service overrides you added as well. Imported
libraries, configuration, source archives and history are retained. Only remove
those separately when you are sure they are no longer needed.
