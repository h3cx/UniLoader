# UniLoader: practical development specification

Status: initial usable release implemented (2026-10-11). This document retains
both the first-release requirements and subsequent improvements below.

Implemented: manual/watched imports, conservative footprint matching and desktop
comparison, process-based project suggestions, global/portable local libraries,
automatic library creation/registration, reference rewriting, model path repair,
pending/retry/history, backups, configuration and the systemd setup guide.

Deferred as planned: authoritative IPC discovery, comparison overlays, tray
controls and guarded undo. Remembered destinations currently last for the running
session; fixed project/global preferences are configured in TOML. Desktop dialogs
use Rust egui/eframe compiled into the executable; rfd uses the XDG desktop portal
for file browsing. Automated fixture tests cover file behavior; live KiCad and
desktop acceptance still need verification in a graphical session.

## Goal and implementation approach

Keep the existing download-and-import workflow, prevent duplicate footprints,
and support portable project libraries alongside global libraries. Ask the user
where a component should go, using open KiCad projects as suggestions.

This is personal development tooling. Favor a small, useful implementation over
a polished application: Linux first, simple dialogs, ordinary files for state,
and a few focused tests. No database, plugin framework, web backend, custom GUI
design system, or full transaction engine is required. Small Rust modules and
egui/eframe dialogs are sufficient. Keep the UI in the same installed binary;
launch an internal dialog process when needed to give its window event loop a
main thread while the watcher continues discovering downloads. Use rfd for file
selection through the desktop portal, with X11 and Wayland support and OpenGL
rendering. No scripting runtime or companion scripts are required.
Preserving originals, avoiding silent overwrites, and keeping footprint links
correct are still required.

## Existing behavior and prerequisites

The current Rust daemon watches `~/Downloads`, recognizes SamacSys,
UltraLibrarian and SnapMagic archives, and writes provider-specific libraries
under one configured directory. Preserve those providers and existing config.

Fix these problems before building new decisions on top of the importer:

- Expand `~` in configured paths and support XDG config/data/cache locations.
- Replace the hand-written symbol scanner with a quote/escape/Unicode-aware
  S-expression reader. Preserve unknown fields and original format versions;
  do not relabel newer content as an older KiCad format.
- Wait for downloads to finish: handle creation and rename/move events, ignore
  temporary downloads, debounce duplicates, and retry temporarily invalid ZIPs
  for a bounded period. File-size stability alone is insufficient.
- Preserve source ZIPs by default. Extract into unique temporary directories;
  clean these after success. Failed imports must remain retryable.
- Include the source path and actual cause in errors rather than silently
  dropping failures. One bad archive must not stop the watcher.
- Stage output, back up changed libraries/tables, and replace individual files
  atomically. On partial failure report what changed and retain recovery files.
  Full crash-atomic multi-file transactions are outside the initial scope.
- Prevent simultaneous writers, including manual imports while the watcher runs.

## Import workflow

1. Detect a completed supported download and extract its component assets.
2. Resolve the destination, prompting unless configuration or an explicit CLI
   option already determines it.
3. Find existing footprint candidates in the destination's effective libraries.
4. Resolve exact matches automatically and ask about uncertain matches.
5. Prepare symbols, footprints, models, reference changes and library-table edits.
6. Validate the plan, write it, then report success or an actionable failure.

Use one common pipeline after provider extraction. A modest split into config,
sources, KiCad parsing, matching, destinations, dialogs and watcher modules is
enough. Provider handlers should discover assets, not independently install them.

Keep a small persisted queue with pending/failed/completed states and source
hashes. Closing a dialog means “later”, not consent. A pending comparison should
not block discovery or independent imports. Serialize writes and show one dialog
at a time. Remember completed imports by source content and destination so repeat
filesystem events are harmless; allow deliberate reimport.

## Destination prompt and open-project discovery

Default watcher policy: ask “Import into project or global libraries?” once per
archive. Show component names, the project name and full directory path.

- One detected project: preselect local import to it; retain Global as an option.
- Several detected projects: show a selector, deduplicating schematic/PCB views
  belonging to the same project. Do not silently guess which project is current.
- None detected: offer Global and Browse for a `.kicad_pro` file.
- Offer Import, Later and Skip, plus “remember for this session/project”.
- Remembering a project preference suppresses prompts only when that project is
  unambiguously the destination. Forget stale selections when a project closes;
  an explicitly configured fixed target remains usable when KiCad is closed.
- Destination preferences do not suppress uncertain-footprint comparisons.

Discovery should use KiCad IPC where available. Enumerate accessible running
instances, query open documents and resolve their project directories. Probe
capabilities instead of assuming all KiCad releases expose the same API.
IPC may need enabling in KiCad; an independent watcher must discover/connect to
instances rather than assume plugin-provided environment variables are present.

KiCad 9/10 IPC is PCB-editor focused; schematic IPC support is documented for
KiCad 11. A project-manager-only session therefore needs a fallback. For a quick
Linux implementation, inspect KiCad process command lines/working directories
for existing project files and label these as suggestions. This is best effort:
startup arguments can be stale, window titles are not reliable full paths, and
unsaved documents may have no usable project. Always retain Browse. Process-based
discovery may ship before IPC if that gets a usable version out sooner.

Freeze the selected destination for each import. A later window/project change
must not redirect an already reviewed import. Missing IPC, an unavailable GUI or
ambiguous detection must leave a pending choice, never silently choose Global.

## Global and project-local libraries

Keep the existing `[libraries].path` as the global storage root. Existing
provider-specific libraries can remain; consolidating or migrating them is not
required. Reuse footprints across providers regardless of storage grouping.

Default project layout:

```text
project/
  project.kicad_pro
  sym-lib-table
  fp-lib-table
  libraries/
    UniLoader.kicad_sym
    UniLoader.pretty/
    models/
```

- Add/merge UniLoader entries into the appropriate symbol and footprint tables.
  Preserve unrelated entries and never silently repoint an occupied nickname.
- Project URIs use `${KIPRJMOD}/libraries/...`; global URIs use stable absolute
  paths. Let users select the KiCad configuration/version directory if ambiguous.
- Resolve project/global precedence and KiCad path variables when searching
  registered libraries. Allow extra search paths; report unreadable libraries and
  unresolved variables. Do not claim a complete search when some roots failed.
- Rewrite each imported symbol's `Footprint` property to its final registered
  `LibraryNickname:FootprintName`. Match associations explicitly; never attach
  every symbol to the first footprint in a multi-component archive.
- Default local mode is portable: reuse an existing project footprint in place;
  copy a selected global/external footprint into the project before referencing it.
- Copy referenced external 3D models when available and rewrite their paths to
  project-relative paths. Preserve model offsets, rotations and scale. List
  missing models or other unresolved dependencies in the import result.
- Do not copy every global library. Only bring in assets needed by the import.
- Do not change existing schematic/PCB instances as a side effect of an import.

If symbol or footprint importing is disabled, honor the existing options. Do not
create a dangling footprint link: resolve it to an available footprint, retain an
already valid link, or leave the component pending with an explanation.

### Automatic library creation

An empty destination must work without manual library setup. Create missing
parent directories, a valid `.kicad_sym` library, the `.pretty` footprint
directory and model storage as needed for the enabled asset types. Create missing
`sym-lib-table` and `fp-lib-table` files in the selected scope and register the
new libraries with matching nicknames. Generate symbols using a supported format
version appropriate to the imported content, not a hardcoded downgrade.

If libraries already exist, merge into them using the conflict policies above.
If a path exists but is not the expected library/directory type, report it instead
of replacing it. Choose a fresh nickname or ask if the intended nickname is
already registered to another library. Global setup must use the selected KiCad
version's configuration directory; project setup must stay inside the selected
project. Missing libraries should never require a trip through KiCad's “Add
Library” dialogs. A subsequent editor refresh/reopen may still be needed.

### 3D model import and broken-path repair

Support STEP (`.step`/`.stp`) and VRML (`.wrl`) assets bundled with supported
provider archives or manual folder imports. Search the extracted bundle
recursively: models need not sit beside their footprints. Import only referenced
models by default. Handle multiple model references per footprint.

For each reference, first try its valid resolved location, then resolve against
the source bundle and configured model search roots. Handle provider-specific
absolute paths, Windows separators/drive prefixes, renamed extraction roots,
and KiCad path variables. A unique matching relative suffix or filename is an
acceptable quick repair strategy. If several files fit, compare content to
collapse identical copies; otherwise ask which model to use rather than choosing
arbitrarily. Do not crawl the whole home directory or fetch models online.

Copy resolved external models into the selected destination's model storage.
Use `${KIPRJMOD}/libraries/models/...` for portable projects and stable absolute
paths for global libraries. Rewrite the footprint's model references, preserving
offset, rotation, scale and other supported settings. Resolve model filename
collisions by content: identical files can share storage; different files need
distinct names. Include models when copying an existing external footprint into
a portable project, not just when importing a fresh archive.

Preserve valid embedded models if present; do not require embedding support for
the initial release. If a model cannot be found, still allow the symbol/footprint
import with a visible warning and mark its model dependency unresolved. Keep the
original reference and record the failure so it can be repaired later. Do not
claim the result is fully portable while external dependencies remain unresolved.
The result should list copied, repaired, reused and missing models.

Add an `options.models` switch, defaulting to enabled, plus configurable model
search roots. Disabling model import must be reported as a portability limitation
when it leaves external references. An initial implementation that repairs common
provider paths and reports difficult cases is sufficient; complete CAD-format
conversion and STEP/VRML geometry analysis are outside scope.

## Footprint matching and duplicate prevention

Search configured/registered libraries and other assets in the same import plan.
An initial in-memory index rebuilt at startup is sufficient; persistent indexing
and sophisticated incremental updates can wait. Recheck selected content before
writing if it may have changed while the dialog was open.

Default decisions:

| Classification | Action |
| --- | --- |
| No plausible candidate after a successful search | Import automatically |
| Conservatively normalized exact match | Reuse automatically |
| Same name, different content | Ask; never overwrite automatically |
| Similar geometry or incomplete comparison | Show candidate comparison |
| Search failure that prevents a reliable decision | Leave pending with details |

“Exact” means parsed content equality after a small, documented normalization
allowlist, such as whitespace, the root footprint name and generated identifiers.
Retain pad numbering, geometry, layers, drills, mask/paste settings, attributes,
graphics and model semantics. Handle model paths in their library context; equal
relative strings from different libraries do not prove equal models. Unknown
constructs must be preserved and must not be discarded to manufacture equality.
An initial stricter matcher that misses some duplicates is acceptable.

Candidate similarity can start with simple rules: pad count/number set, pad types,
positions, sizes, shapes, layers, drill sizes and overall dimensions. Names and
package hints help rank candidates but do not establish equivalence. Equal pads
with different courtyard, mask or model content remain a comparison case.
Rotation-invariant matching and advanced geometry analysis can wait.

When several exact matches exist, prefer a destination-local copy, then a stable
configured library order. In portable mode, copy an external match locally.

The comparison dialog must show:

- Incoming footprint and selectable candidates with library/name/path.
- Side-by-side previews at the same scale, with pad numbers and dimensions.
- A short difference list, particularly numbering, pitch, drills and pad sizes.
- Use selected existing footprint, Import separately, and Later/Skip actions.

A simple SVG preview in a helper window is acceptable. Unsupported drawing
elements should be indicated; users can open the files in KiCad for inspection.
Overlay rendering is desirable after the basic comparison works. Do not require
a complete KiCad rendering engine for the first release.

Separate imports need a deterministic collision-free name and corresponding
symbol-reference updates. Remember user mappings using incoming and selected
content fingerprints; invalidate them if either footprint changes.

Expose matching enable/disable, search roots, exact-match policy, similarity
tolerances, ambiguous-match policy and remembered mappings in configuration.
Disabling matching must still prevent silent filename overwrites. Headless
policies can keep separate copies or skip; default ambiguity handling is pending.

## CLI, configuration and small usability additions

Keep no-argument watcher startup working. Add these proposed commands:

```text
uniloader watch
uniloader import component.zip --global
uniloader import component.zip --project /path/to/project.kicad_pro
uniloader import extracted-component/ --project /path/to/project.kicad_pro
uniloader import component.zip --project /path/to/project.kicad_pro --dry-run
uniloader pending
uniloader retry <import-id>
```

`--dry-run` reports proposed destinations, reuse/copy decisions, unresolved
choices and table/reference changes without writing libraries or remembering
decisions. Add CLI configuration override and a noninteractive mode that cannot
hang waiting for dialogs.

Configuration should cover watch directories, global root, destination policy
(`ask`, `global`, `project`), fixed project, portable local mode, enabled asset
types, matching policies, GUI availability and source retention. Preserve the
existing minimal TOML configuration with sensible defaults. Update the complete
example alongside implementation rather than documenting speculative keys.

Treat symbol-name collisions explicitly: skip identical symbols; ask or create
a separate correctly renamed symbol for different content. Preserve internal
unit names/inheritance when renaming. No automatic destructive replacement.

Maintain a small import history recording source, destination, decisions and
changed files/backups. A tray destination selector and “undo last import” are
follow-ups. Undo must refuse to overwrite subsequent edits; manual backup restore
instructions are sufficient initially. No full library-management UI is needed.

## Setup guide and systemd user service

Implementation must include `docs/SETUP.md`, linked from the README, and a shipped
example `systemd/uniloader.service`. The guide must document:

1. Linux build/runtime dependencies, supported KiCad versions, graphics/portal needs,
   and any IPC configuration. State limitations of process-based discovery.
2. `cargo build --release --locked` and installation of the executable to
   `~/.local/bin/uniloader`; no root installation required.
3. Minimal and full configuration, watched directories, library search scope,
   global/local examples, model search/path repair, automatic library creation,
   and how remembered decisions are reset.
4. A foreground smoke test before enabling automatic startup.
5. User service installation, startup, logs, restart, disable and removal.
6. Desktop session environment troubleshooting for X11/Wayland and how to
   resume pending choices if dialogs could not be displayed.
7. Updating the binary/config, restoring backups, and known KiCad library refresh
   limitations. Explain when a library/editor reload is needed.

Use a systemd **user** service, not a system service or root daemon. The intended
meaning of “always running” is throughout the user's graphical login session.
Start from the graphical session so dialogs have the right display environment.
Initial unit template, to verify with the final CLI:

```ini
[Unit]
Description=UniLoader KiCad component importer
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/uniloader watch
Restart=on-failure
RestartSec=3

[Install]
WantedBy=graphical-session.target
```

The guide must provide copy/install commands and explain:

```sh
systemctl --user daemon-reload
systemctl --user enable --now uniloader.service
systemctl --user status uniloader.service
journalctl --user -u uniloader.service -f
systemctl --user restart uniloader.service
systemctl --user disable --now uniloader.service
```

Some compositors do not manage `graphical-session.target` or propagate display
variables automatically. Document a session-start hook that imports the actual
session's `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY` and relevant desktop variables
when set, then starts the service. Do not hardcode another session's display or
tell users that enabling the unit alone fixes all desktops.

Running before login/after logout is optional later work. Explain that lingering
does not provide a graphical session: unattended operation would require a
configured noninteractive destination and persistent pending decisions.

## Delivery order and acceptance

### First usable release

Fix parser/path/download issues; preserve archives; introduce the common import
plan; implement manual global/project imports, table registration and reference
rewriting. Add the destination popup with best-effort discovery and Browse.
Add strict exact matching and basic candidate comparison. Ship setup docs and
the user service. Basic model copying belongs here so portable mode is useful;
report unsupported/missing dependencies clearly.

### Subsequent improvements

Broader IPC discovery, refined similarity rules, comparison overlays, tray
controls, history UI/guarded undo, and more complete model handling. Avoid making
these prerequisites for a working first release.

Use a small fixture set, with at least one representative archive per provider.
Focused automated checks should cover quoted parentheses/Unicode, reference
rewriting, exact reuse, same-name conflicts, table preservation and portable
paths. Manual acceptance should demonstrate:

- Reimporting the same archive creates no duplicate footprint or symbol.
- An exact match from another provider is reused without a comparison prompt.
- A near match opens comparison; choosing either action produces valid links.
- Zero/one/multiple discovered projects produce the specified destination choices.
- Copying a project elsewhere and opening it without the original custom global
  libraries resolves imported symbols, footprints and available copied models.
- Importing into an empty destination creates and registers valid libraries.
- A bundled model with an obsolete absolute/Windows path is found, copied and
  linked correctly; verify it in KiCad's 3D viewer. Multiple different models
  with the same basename are not silently confused.
- Missing models are reported without discarding the usable symbol/footprint.
- Partial downloads, invalid archives and closed dialogs remain recoverable.
- The user service restarts after failure and shows dialogs in a real desktop
  session. Verify with KiCad; file-only tests do not establish live integration.

## References

- [KiCad IPC capabilities, connection and version limitations](https://dev-docs.kicad.org/en/apis-and-binding/ipc-api/for-addon-developers/)
- [KiCad footprint libraries and project-relative paths](https://docs.kicad.org/9.0/en/pcbnew/pcbnew.html#_path_variable_substitution)
- [KiCad S-expression format](https://dev-docs.kicad.org/en/file-formats/sexpr-intro/)
- [systemd desktop-session integration](https://github.com/systemd/systemd/blob/main/docs/DESKTOP_ENVIRONMENTS.md)
