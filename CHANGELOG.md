# Changelog

## 0.2.2

- Use `UniLoader_Global` and `UniLoader_Local` nicknames for global and project libraries, preventing project entries from shadowing global managed libraries.
- Keep existing asset filenames and portable paths unchanged.
- Add `doctor --global` and `doctor --project FILE` to preview nickname repairs; `--fix` backs up and migrates registrations, managed symbol bindings, and project schematic/PCB references.
- Preserve unrelated registrations, reject occupied target nicknames before writes, and retain exact legacy nickname mappings for later project migrations.
- Stop imports using legacy managed nicknames with instructions to run doctor.

Validation: 62 automated tests, Clippy, formatting, and release build passed. Live KiCad integration requires manual verification.

## 0.2.1

- Fix high idle watcher CPU usage caused by directory scans generating filesystem open events and triggering further scans.
- Ignore read/open access events before queuing watcher work, while retaining write-close events and filesystem overflow recovery.
- Batch queued filesystem events into one scan while preserving periodic reconciliation and partial-download retries.
- Add a Linux idle CPU regression check alongside the download, rename, duplicate and restart integration checks.

Validation: 59 automated tests, Clippy, formatting, and release build passed. Local Linux idle profiling confirmed the watcher no longer continuously rescans its download directory.

## 0.2.0

- Import components into global or portable project libraries, creating and registering libraries when needed.
- Reuse confidently matching footprints and compare uncertain matches in native Rust dialogs.
- Discover possible open KiCad projects and choose a destination for watched downloads.
- Copy 3D models into managed libraries, repair model paths, and resolve ambiguous model choices.
- Keep pending imports and history, retry failures, and back up modified library files.
- Bundle egui/eframe dialogs and footprint previews in the executable, with rfd file selection.
- Document setup and a persistent user systemd service.

Validation: 58 automated tests, Clippy, formatting, and release build passed. Live desktop dialogs, portal file pickers, and KiCad integration still require manual verification.

## 0.1.0

- Initial component importer release.
