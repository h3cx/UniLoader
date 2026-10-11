# Changelog

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
