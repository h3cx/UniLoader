# UniLoader

UniLoader imports KiCad components from downloaded ZIPs or extracted folders,
including [SamacSys](https://componentsearchengine.com/),
[UltraLibrarian](https://app.ultralibrarian.com/search) and
[SnapMagic](https://www.snapeda.com/home/). Run it manually or watch your download
folder throughout your graphical login session.

It supports symbols, footprints and referenced STEP/VRML models, with:

- Global (`UniLoader_Global`) and project-local (`UniLoader_Local`) library nicknames, created and registered automatically.
- A destination dialog offering detected KiCad project suggestions, Global and Browse.
- Conservative exact footprint reuse and comparison dialogs for uncertain matches.
- Broken model-path repair, explicit ambiguous model choices and missing-model warnings.
- Preserved source archives, pending import retries, backups and import history.

## Quick start

Linux requires Rust 1.95 or newer and the usual
desktop graphics libraries. The binary includes egui/eframe dialogs; rfd file
browsing uses the desktop's XDG portal. `notify-send` is optional. See the
[setup guide](docs/SETUP.md) for packages, configuration, recovery and the shipped
[systemd user service](systemd/uniloader.service).

```sh
cargo build --release --locked
install -Dm755 target/release/uniloader ~/.local/bin/uniloader
~/.local/bin/uniloader import /path/to/component.zip --project /path/to/Board/Board.kicad_pro --dry-run
~/.local/bin/uniloader import /path/to/component.zip --project /path/to/Board/Board.kicad_pro
~/.local/bin/uniloader watch
```

A default configuration is generated on first launch at
`${XDG_CONFIG_HOME:-~/.config}/uniloader/config.toml`. The existing minimal config
remains supported:

```toml
[libraries]
path = "~/KiCad/libraries"
# For global imports, choose your actual installed KiCad version if detection
# finds none or more than one:
# kicad_config = "~/.config/kicad/9.0"
```

See [the complete configuration example](config/config_full.toml). Project
imports create libraries beside the `.kicad_pro` file using `${KIPRJMOD}` paths;
KiCad may need a library refresh or reopen. Missing models are reported and mean
that a component is not yet fully portable.

```sh
uniloader import /path/to/component.zip --global
uniloader import /path/to/extracted-folder --project /path/to/Board/Board.kicad_pro
uniloader pending
uniloader retry IMPORT_ID
uniloader discover
```

No-argument startup also runs the watcher. `--noninteractive` leaves unresolved
choices pending; `--force` on manual imports deliberately revisits a completed
source. Check an imported component in KiCad before using it in a board.

## Upgrade existing libraries to v0.2.2

Close KiCad and stop the watcher, then preview and apply the nickname migration:

```sh
uniloader doctor --global
uniloader doctor --global --fix
uniloader doctor --project /path/to/Board/Board.kicad_pro
uniloader doctor --project /path/to/Board/Board.kicad_pro --fix
```

Repeat the project commands for each existing project. Project repair also checks
and repairs the configured global libraries. Existing library filenames stay the
same. Changed files are backed up; see the [setup guide](docs/SETUP.md#nickname-migration-v022)
for migration scope and recovery. Reopen KiCad afterwards.

## Scope

This is practical Linux development tooling. Project discovery currently uses
process arguments and working directories as suggestions; check the selected
path. KiCad IPC discovery, tray controls, comparison overlays and automatic undo
are follow-ups. Legacy `.lib` conversion and CAD-format conversion are outside
the current release. The preview is approximate; inspect unusual footprints in
KiCad. The [specification](SPEC.md) records the intended behavior and delivery
priorities.

## Warranty

Use this program at your own risk, no warranty is provided for anything that it
could do to your libraries, KiCad or operating system. If you encounter any issues
feel free to open an issue. Licensed under [GPL-3.0-or-later](LICENSE).
