//! End-to-end CLI fixtures: all filesystem and process state is isolated.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};
#[path = "../src/sexpr.rs"]
#[allow(dead_code)]
mod sexpr;
use sexpr::{Node, parse};

struct Harness {
    _tmp: TempDir,
    root: PathBuf,
    project: PathBuf,
    config: PathBuf,
}
impl Harness {
    fn new(policy: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let project = root.join("project/board.kicad_pro");
        fs::create_dir_all(project.parent().unwrap()).unwrap();
        fs::write(&project, "{}").unwrap();
        let config = root.join("config.toml");
        let h = Self {
            _tmp: temp,
            root,
            project,
            config,
        };
        h.policy(policy);
        h
    }
    fn policy(&self, policy: &str) {
        fs::write(&self.config, format!("[libraries]\npath = {:?}\nkicad_config = {:?}\n[matching]\nambiguous = {policy:?}\n", self.root.join("global").to_str().unwrap(), self.root.join("kicad-config").to_str().unwrap())).unwrap();
    }
    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_uniloader"))
            .args([
                "--config",
                self.config.to_str().unwrap(),
                "--noninteractive",
            ])
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg-config"))
            .env("XDG_DATA_HOME", self.root.join("xdg-data"))
            .env("XDG_CACHE_HOME", self.root.join("xdg-cache"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
    fn import(&self, source: &Path, extra: &[&str]) -> Output {
        let mut args = vec![
            "import",
            source.to_str().unwrap(),
            "--project",
            self.project.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        self.cmd(&args)
    }
    fn bundle(&self, name: &str, files: &[(&str, String)]) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        for (file, text) in files {
            let p = path.join(file);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        path
    }
    fn archive(&self, name: &str, files: &[(&str, String)]) -> PathBuf {
        let path = self.root.join(name);
        let mut zip = ZipWriter::new(fs::File::create(&path).unwrap());
        for (file, text) in files {
            zip.start_file(*file, SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }
    fn library(&self) -> Node {
        parse(&fs::read_to_string(self.root.join("project/libraries/UniLoader.kicad_sym")).unwrap())
            .unwrap()
    }
    fn footprints(&self) -> Vec<PathBuf> {
        let dir = self.root.join("project/libraries/UniLoader.pretty");
        if !dir.exists() {
            return vec![];
        }
        let mut paths: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "kicad_mod"))
            .collect();
        paths.sort();
        paths
    }
}
fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn footprint(name: &str, width: f64, extra: &str) -> String {
    format!(
        r#"(footprint "{name}" (version 20240108) (generator "fixture") (layer "F.Cu") (pad "1" smd rect (at 0 0) (size {width} 1) (layers "F.Cu" "F.Paste" "F.Mask")) {extra})"#
    )
}
fn symbol(name: &str, fp: &str) -> String {
    format!(
        r#"(kicad_symbol_lib (version 20231120) (generator "fixture") (symbol "{name}" (property "Reference" "U") (property "Value" "{name}") (property "Footprint" "{fp}") (symbol "{name}_0_1" (text "µ Ω (quoted)" (at 0 0 0) (effects (font (size 1 1)))))))"#
    )
}
fn symbols(n: &Node) -> Vec<&Node> {
    n.children()
        .iter()
        .filter(|n| n.head() == Some("symbol"))
        .collect()
}
fn link(n: &Node) -> &str {
    n.children()
        .iter()
        .find(|n| n.head() == Some("property") && n.value() == Some("Footprint"))
        .unwrap()
        .children()[2]
        .atom()
        .unwrap()
}

#[test]
fn imports_three_provider_layouts_and_preserves_original_archives() {
    for (archive, sym_path, fp_path, readme) in [
        (
            "LIB_123.zip",
            "SamacSys_Parts/KiCad/Parts.kicad_sym",
            "SamacSys_Parts/KiCad/Package.kicad_mod",
            "SamacSys",
        ),
        (
            "ul_123.zip",
            "KiCAD/component.kicad_sym",
            "KiCAD/footprints.pretty/Package.kicad_mod",
            "UltraLibrarian",
        ),
        (
            "snapmagic.zip",
            "Component.kicad_sym",
            "Component.pretty/Package.kicad_mod",
            "SnapEDA",
        ),
    ] {
        let h = Harness::new("ask");
        let source = h.archive(
            archive,
            &[
                (sym_path, symbol("Part", "OldLib:Package")),
                (fp_path, footprint("Package", 1.0, "")),
                ("How-to-import.txt", readme.into()),
            ],
        );
        ok(&h.import(&source, &[]));
        assert!(source.is_file());
        assert_eq!(h.footprints().len(), 1);
        let lib = h.library();
        assert_eq!(link(symbols(&lib)[0]), "UniLoader_Local:Package");
        assert!(lib.to_string().contains("µ Ω (quoted)"));
        assert_eq!(lib.field("version").unwrap().value(), Some("20231120"));
    }
}

#[test]
fn exact_cross_provider_reuse_and_repeat_import_are_idempotent() {
    let h = Harness::new("ask");
    let a = h.archive(
        "LIB_a.zip",
        &[
            ("a.kicad_sym", symbol("First", "PkgA")),
            ("PkgA.kicad_mod", footprint("PkgA", 1.0, "(uuid a)")),
        ],
    );
    let b = h.archive(
        "ul_b.zip",
        &[
            ("b.kicad_sym", symbol("Second", "PkgB")),
            ("PkgB.kicad_mod", footprint("PkgB", 1.0, "(uuid b)")),
        ],
    );
    ok(&h.import(&a, &[]));
    ok(&h.import(&b, &[]));
    assert_eq!(h.footprints().len(), 1);
    let lib = h.library();
    assert_eq!(symbols(&lib).len(), 2);
    assert!(
        symbols(&lib)
            .iter()
            .all(|s| link(s) == "UniLoader_Local:PkgA")
    );
    ok(&h.import(&a, &[]));
    ok(&h.import(&b, &["--force"]));
    assert_eq!(symbols(&h.library()).len(), 2);
    assert_eq!(h.footprints().len(), 1);
}

#[test]
fn uncertain_footprints_stay_pending_then_retry_with_separate_policy() {
    let h = Harness::new("ask");
    let first = h.bundle(
        "first",
        &[
            ("a.kicad_sym", symbol("First", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let second = h.bundle(
        "second",
        &[
            ("b.kicad_sym", symbol("Second", "Package")),
            ("Package.kicad_mod", footprint("Package", 2.0, "")),
        ],
    );
    ok(&h.import(&first, &[]));
    let result = h.import(&second, &[]);
    assert!(output(&result).contains("pending"), "{}", output(&result));
    assert_eq!(h.footprints().len(), 1);
    assert_eq!(symbols(&h.library()).len(), 1);
    let listing = h.cmd(&["pending"]);
    ok(&listing);
    assert!(output(&listing).contains("second"));
    let records = h.root.join("xdg-data/uniloader/imports");
    let pending: serde_json::Value = fs::read_dir(records)
        .unwrap()
        .map(|e| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(e.unwrap().path()).unwrap())
                .unwrap()
        })
        .find(|v| v["source"].as_str().is_some_and(|s| s.ends_with("second")))
        .unwrap();
    h.policy("separate");
    ok(&h.cmd(&["retry", pending["id"].as_str().unwrap()]));
    assert_eq!(h.footprints().len(), 2);
    let lib = h.library();
    let second = symbols(&lib)
        .into_iter()
        .find(|s| s.value() == Some("Second"))
        .unwrap();
    assert!(link(second).starts_with("UniLoader_Local:Package_"));
}

#[test]
fn dry_run_does_not_create_libraries_or_tables() {
    let h = Harness::new("ask");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let result = h.import(&source, &["--dry-run"]);
    ok(&result);
    assert!(output(&result).contains("Would write"));
    assert!(!h.root.join("project/libraries").exists());
    assert!(!h.root.join("project/sym-lib-table").exists());
    assert!(!h.root.join("project/fp-lib-table").exists());
}

#[test]
fn preserves_existing_tables_and_unknown_symbol_metadata() {
    let h = Harness::new("ask");
    fs::create_dir_all(h.root.join("other.pretty")).unwrap();
    fs::write(h.root.join("project/fp-lib-table"),format!(r#"(fp_lib_table (version 7) (lib (name "Other") (type "KiCad") (uri "{}") (options "") (descr "Keep µ")))"#,h.root.join("other.pretty").display())).unwrap();
    fs::write(h.root.join("project/sym-lib-table"),r#"(sym_lib_table (version 7) (lib (name "Other") (type "KiCad") (uri "${KIPRJMOD}/other.kicad_sym") (options "") (descr "Keep symbols")))"#).unwrap();
    let mut sym = symbol("Part", "Package");
    sym.insert_str(
        sym.find("(symbol \"Part\"").unwrap(),
        "(future_library (payload \"Ω (keep)\")) ",
    );
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", sym),
            (
                "Package.kicad_mod",
                footprint("Package", 1.0, "(future_footprint (uuid meaningful))"),
            ),
        ],
    );
    ok(&h.import(&source, &[]));
    assert!(h.library().field("future_library").is_some());
    assert!(
        fs::read_to_string(&h.footprints()[0])
            .unwrap()
            .contains("future_footprint")
    );
    for table in ["fp-lib-table", "sym-lib-table"] {
        let table =
            parse(&fs::read_to_string(h.root.join("project").join(table)).unwrap()).unwrap();
        assert_eq!(
            table
                .children()
                .iter()
                .filter(|n| n.head() == Some("lib"))
                .count(),
            2
        );
        assert!(table.to_string().contains("Other"));
    }
}

#[test]
fn repairs_broken_windows_models_and_reports_missing_dependencies() {
    let h = Harness::new("ask");
    let model = r#"(model "C:\\obsolete\\provider\\part.STEP" (offset (xyz 1 2 3)) (rotate (xyz 0 0 90)) (scale (xyz 1 1 1))) (model "missing.wrl")"#;
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, model)),
            ("deep/models/part.STEP", "STEP TEST CONTENT".into()),
        ],
    );
    let result = h.import(&source, &[]);
    ok(&result);
    assert!(output(&result).contains("Missing model"));
    let fp = fs::read_to_string(&h.footprints()[0]).unwrap();
    assert!(fp.contains("${KIPRJMOD}/libraries/models/part_"));
    assert!(fp.contains("(xyz 1 2 3)"));
    assert!(fp.contains("missing.wrl"));
    let models: Vec<_> = fs::read_dir(h.root.join("project/libraries/models"))
        .unwrap()
        .collect();
    assert_eq!(models.len(), 1);
}

#[test]
fn multi_symbol_footprint_bindings_do_not_use_first_footprint_for_all() {
    let h = Harness::new("separate");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("First", "PackageA")),
            ("b.kicad_sym", symbol("Second", "PackageB")),
            ("PackageA.kicad_mod", footprint("PackageA", 1.0, "")),
            ("PackageB.kicad_mod", footprint("PackageB", 2.0, "")),
        ],
    );
    ok(&h.import(&source, &[]));
    let lib = h.library();
    for (name, expected) in [
        ("First", "UniLoader_Local:PackageA"),
        ("Second", "UniLoader_Local:PackageB"),
    ] {
        let s = symbols(&lib)
            .into_iter()
            .find(|s| s.value() == Some(name))
            .unwrap();
        assert_eq!(link(s), expected);
    }
}

#[test]
fn creates_and_registers_empty_global_destination() {
    let h = Harness::new("ask");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    ok(&h.cmd(&["import", source.to_str().unwrap(), "--global"]));
    for table in ["sym-lib-table", "fp-lib-table"] {
        let text = fs::read_to_string(h.root.join("kicad-config").join(table)).unwrap();
        assert!(text.contains("UniLoader"));
        assert!(text.contains(h.root.join("global").to_str().unwrap()));
        assert!(!text.contains("${KIPRJMOD}"));
    }
    assert!(!h.root.join("project/libraries").exists());
}

#[test]
fn same_plan_duplicate_footprints_with_models_reuse_one_copy() {
    let h = Harness::new("ask");
    let model = r#"(model "part.step" (offset (xyz 0 0 0)))"#;
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("First", "PackageA")),
            ("b.kicad_sym", symbol("Second", "PackageB")),
            ("PackageA.kicad_mod", footprint("PackageA", 1.0, model)),
            ("PackageB.kicad_mod", footprint("PackageB", 1.0, model)),
            ("part.step", "SAME STEP MODEL".into()),
        ],
    );
    ok(&h.import(&source, &[]));
    assert_eq!(h.footprints().len(), 1);
    let lib = h.library();
    assert_eq!(symbols(&lib).len(), 2);
    assert_eq!(link(symbols(&lib)[0]), link(symbols(&lib)[1]));
}

#[test]
fn symbol_conflict_renames_units_and_force_reimport_stays_idempotent() {
    let h = Harness::new("separate");
    let first = h.bundle(
        "first",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let modified = symbol("Part", "Package").replace("µ Ω (quoted)", "different symbol geometry");
    let second = h.bundle(
        "second",
        &[
            ("a.kicad_sym", modified),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    ok(&h.import(&first, &[]));
    ok(&h.import(&second, &[]));
    let lib = h.library();
    assert_eq!(symbols(&lib).len(), 2);
    let renamed = symbols(&lib)
        .into_iter()
        .find(|s| s.value() != Some("Part"))
        .unwrap();
    let renamed_name = renamed.value().unwrap();
    assert!(renamed_name.starts_with("Part_"));
    assert_eq!(
        renamed.field("symbol").unwrap().value().unwrap(),
        format!("{renamed_name}_0_1")
    );
    ok(&h.import(&second, &["--force"]));
    assert_eq!(symbols(&h.library()).len(), 2);
}

#[test]
fn portable_project_copies_exact_global_footprint_and_model() {
    let h = Harness::new("ask");
    let model = r#"(model "part.step" (offset (xyz 0 0 0)))"#;
    let global = h.bundle(
        "global-source",
        &[
            ("a.kicad_sym", symbol("GlobalPart", "GlobalPackage")),
            (
                "GlobalPackage.kicad_mod",
                footprint("GlobalPackage", 1.0, model),
            ),
            ("part.step", "PORTABLE MODEL".into()),
        ],
    );
    ok(&h.cmd(&["import", global.to_str().unwrap(), "--global"]));
    let local = h.bundle(
        "local-source",
        &[
            ("a.kicad_sym", symbol("LocalPart", "LocalPackage")),
            (
                "LocalPackage.kicad_mod",
                footprint("LocalPackage", 1.0, model),
            ),
            ("part.step", "PORTABLE MODEL".into()),
        ],
    );
    ok(&h.import(&local, &[]));
    assert_eq!(h.footprints().len(), 1);
    let text = fs::read_to_string(&h.footprints()[0]).unwrap();
    assert!(text.contains("${KIPRJMOD}/libraries/models/"));
    assert!(!text.contains(h.root.join("global").to_str().unwrap()));
    let copied_models: Vec<_> = fs::read_dir(h.root.join("project/libraries/models"))
        .unwrap()
        .collect();
    assert_eq!(copied_models.len(), 1);
    assert_eq!(
        link(symbols(&h.library())[0]),
        "UniLoader_Local:GlobalPackage"
    );
}

fn records(h: &Harness) -> Vec<serde_json::Value> {
    fs::read_dir(h.root.join("xdg-data/uniloader/imports"))
        .unwrap()
        .map(|e| serde_json::from_slice(&fs::read(e.unwrap().path()).unwrap()).unwrap())
        .collect()
}
fn append_config(h: &Harness, text: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(&h.config).unwrap();
    writeln!(file, "{text}").unwrap();
}

#[test]
fn symbol_only_global_dependency_is_copied_into_portable_project() {
    let h = Harness::new("ask");
    let global = h.bundle(
        "global-source",
        &[
            ("a.kicad_sym", symbol("GlobalPart", "Package")),
            (
                "Package.kicad_mod",
                footprint("Package", 1.0, r#"(model "part.step")"#),
            ),
            ("part.step", "GLOBAL MODEL".into()),
        ],
    );
    ok(&h.cmd(&["import", global.to_str().unwrap(), "--global"]));
    let source = h.bundle(
        "symbol-only",
        &[(
            "b.kicad_sym",
            symbol("LocalPart", "UniLoader_Global:Package"),
        )],
    );
    ok(&h.import(&source, &[]));
    assert_eq!(h.footprints().len(), 1);
    assert_eq!(link(symbols(&h.library())[0]), "UniLoader_Local:Package");
    assert!(
        fs::read_to_string(&h.footprints()[0])
            .unwrap()
            .contains("${KIPRJMOD}/libraries/models/")
    );
    assert_eq!(
        fs::read_dir(h.root.join("project/libraries/models"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn same_archive_keeps_completed_history_for_each_destination() {
    let h = Harness::new("ask");
    let source = h.archive(
        "LIB_multi.zip",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let second = h.root.join("second/second.kicad_pro");
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    fs::write(&second, "{}").unwrap();
    ok(&h.import(&source, &[]));
    ok(&h.cmd(&[
        "import",
        source.to_str().unwrap(),
        "--project",
        second.to_str().unwrap(),
    ]));
    let all = records(&h);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r["status"] == "completed"));
    assert_ne!(all[0]["destination"], all[1]["destination"]);
    let again = h.import(&source, &[]);
    ok(&again);
    assert!(output(&again).contains("Already imported"));
    assert_eq!(records(&h).len(), 2);
}

#[test]
fn project_library_and_model_paths_resolve_after_directory_relocation() {
    let h = Harness::new("ask");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            (
                "Package.kicad_mod",
                footprint("Package", 1.0, r#"(model "part.step")"#),
            ),
            ("part.step", "RELOCATED MODEL".into()),
        ],
    );
    ok(&h.import(&source, &[]));
    let relocated = h.root.join("relocated project");
    fs::rename(h.root.join("project"), &relocated).unwrap();
    for file in ["sym-lib-table", "fp-lib-table"] {
        let table = parse(&fs::read_to_string(relocated.join(file)).unwrap()).unwrap();
        for lib in table.children().iter().filter(|n| n.head() == Some("lib")) {
            let uri = lib.field("uri").unwrap().value().unwrap();
            assert!(uri.starts_with("${KIPRJMOD}/"));
            assert!(
                relocated
                    .join(uri.strip_prefix("${KIPRJMOD}/").unwrap())
                    .exists()
            );
        }
    }
    let fp = parse(
        &fs::read_to_string(relocated.join("libraries/UniLoader.pretty/Package.kicad_mod"))
            .unwrap(),
    )
    .unwrap();
    let uri = fp.field("model").unwrap().value().unwrap();
    let path = relocated.join(uri.strip_prefix("${KIPRJMOD}/").unwrap());
    assert_eq!(fs::read(path).unwrap(), b"RELOCATED MODEL");
    let lib = parse(&fs::read_to_string(relocated.join("libraries/UniLoader.kicad_sym")).unwrap())
        .unwrap();
    assert_eq!(link(symbols(&lib)[0]), "UniLoader_Local:Package");
}

#[test]
fn disabled_footprint_import_reuses_registered_assets_and_rejects_dangling_links() {
    let h = Harness::new("ask");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    ok(&h.import(&source, &[]));
    append_config(&h, "[options]\nfootprints = false");
    let valid = h.bundle(
        "valid",
        &[("b.kicad_sym", symbol("Second", "UniLoader_Local:Package"))],
    );
    ok(&h.import(&valid, &[]));
    let invalid = h.bundle(
        "invalid",
        &[("c.kicad_sym", symbol("Third", "Missing:NoSuchPackage"))],
    );
    let result = h.import(&invalid, &[]);
    assert!(!result.status.success());
    assert!(output(&result).contains("pending"));
    assert_eq!(symbols(&h.library()).len(), 2);
    assert_eq!(h.footprints().len(), 1);
}

#[test]
fn same_model_filename_with_different_content_gets_distinct_destinations() {
    let h = Harness::new("separate");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("First", "PackageA")),
            ("b.kicad_sym", symbol("Second", "PackageB")),
            (
                "PackageA.kicad_mod",
                footprint("PackageA", 1.0, r#"(model "a/part.step")"#),
            ),
            (
                "PackageB.kicad_mod",
                footprint("PackageB", 2.0, r#"(model "b/part.step")"#),
            ),
            ("a/part.step", "MODEL A".into()),
            ("b/part.step", "MODEL B".into()),
        ],
    );
    ok(&h.import(&source, &[]));
    let mut model_contents: Vec<_> = fs::read_dir(h.root.join("project/libraries/models"))
        .unwrap()
        .map(|e| fs::read(e.unwrap().path()).unwrap())
        .collect();
    model_contents.sort();
    assert_eq!(
        model_contents,
        vec![b"MODEL A".to_vec(), b"MODEL B".to_vec()]
    );
    let paths: Vec<_> = h
        .footprints()
        .iter()
        .map(|p| {
            parse(&fs::read_to_string(p).unwrap())
                .unwrap()
                .field("model")
                .unwrap()
                .value()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(paths.len(), 2);
    assert_ne!(paths[0], paths[1]);
}

#[test]
fn retry_keeps_reviewed_project_when_default_destination_changes() {
    let h = Harness::new("ask");
    let first = h.bundle(
        "first",
        &[
            ("a.kicad_sym", symbol("First", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let second = h.bundle(
        "second",
        &[
            ("b.kicad_sym", symbol("Second", "Package")),
            ("Package.kicad_mod", footprint("Package", 2.0, "")),
        ],
    );
    ok(&h.import(&first, &[]));
    assert!(!h.import(&second, &[]).status.success());
    let pending = records(&h)
        .into_iter()
        .find(|r| r["status"] == "pending")
        .unwrap();
    let other = h.root.join("other/other.kicad_pro");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, "{}").unwrap();
    h.policy("separate");
    append_config(
        &h,
        &format!(
            "[destination]\npolicy = \"project\"\nproject = {:?}",
            other.to_str().unwrap()
        ),
    );
    ok(&h.cmd(&["retry", pending["id"].as_str().unwrap()]));
    assert_eq!(h.footprints().len(), 2);
    assert!(!other.parent().unwrap().join("libraries").exists());
}

#[test]
fn registered_footprint_uses_filename_as_library_identifier() {
    let h = Harness::new("ask");
    let pretty = h.root.join("registered.pretty");
    fs::create_dir_all(&pretty).unwrap();
    fs::write(
        pretty.join("Actual.kicad_mod"),
        footprint("InternalOldName", 1.0, ""),
    )
    .unwrap();
    fs::write(h.root.join("project/fp-lib-table"),format!(r#"(fp_lib_table (lib (name "External") (type "KiCad") (uri "{}") (options "") (descr "")))"#,pretty.display())).unwrap();
    // Keep the library inside the project to permit in-place portable reuse.
    let local_pretty = h.root.join("project/registered.pretty");
    fs::rename(&pretty, &local_pretty).unwrap();
    fs::write(h.root.join("project/fp-lib-table"),format!(r#"(fp_lib_table (lib (name "External") (type "KiCad") (uri "{}") (options "") (descr "")))"#,local_pretty.display())).unwrap();
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Incoming")),
            ("Incoming.kicad_mod", footprint("Incoming", 1.0, "")),
        ],
    );
    ok(&h.import(&source, &[]));
    assert_eq!(link(symbols(&h.library())[0]), "External:Actual");
}

#[test]
fn disabled_footprints_cannot_reference_an_unregistered_managed_directory() {
    let h = Harness::new("ask");
    append_config(&h, "[options]\nfootprints = false");
    let pretty = h.root.join("project/libraries/UniLoader.pretty");
    fs::create_dir_all(&pretty).unwrap();
    fs::write(
        pretty.join("Package.kicad_mod"),
        footprint("Package", 1.0, ""),
    )
    .unwrap();
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let result = h.import(&source, &[]);
    // Registration without copying is also acceptable, but a successful import
    // must never write a symbol link whose library nickname cannot be resolved.
    if result.status.success() {
        let table = parse(
            &fs::read_to_string(h.root.join("project/fp-lib-table"))
                .expect("successful import needs footprint registration"),
        )
        .unwrap();
        assert!(table.children().iter().any(|n| n.head() == Some("lib")
            && n.field("name").and_then(Node::value) == Some("UniLoader_Local")));
    } else {
        assert!(output(&result).contains("pending"));
        assert!(
            !h.root
                .join("project/libraries/UniLoader.kicad_sym")
                .exists()
        );
    }
}

#[test]
fn qualified_existing_reference_is_not_silently_retargeted_by_bundle_basename() {
    let h = Harness::new("separate");
    let pretty = h.root.join("project/existing.pretty");
    fs::create_dir_all(&pretty).unwrap();
    fs::write(
        pretty.join("Package.kicad_mod"),
        footprint("Package", 5.0, ""),
    )
    .unwrap();
    fs::write(h.root.join("project/fp-lib-table"),format!(r#"(fp_lib_table (lib (name "Existing") (type "KiCad") (uri "{}") (options "") (descr "")))"#,pretty.display())).unwrap();
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("First", "Incoming:Package")),
            ("b.kicad_sym", symbol("Second", "Existing:Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    let result = h.import(&source, &[]);
    if result.status.success() {
        let lib = h.library();
        let second = symbols(&lib)
            .into_iter()
            .find(|s| s.value() == Some("Second"))
            .unwrap();
        assert_eq!(link(second), "Existing:Package");
    } else {
        assert!(output(&result).contains("pending"));
    }
}

#[test]
fn doctor_migrates_both_scopes_and_project_references_with_backups() {
    let h = Harness::new("separate");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    ok(&h.cmd(&["import", source.to_str().unwrap(), "--global"]));
    ok(&h.import(&source, &[]));
    // Recreate v0.2.1 registrations and stored footprint bindings.
    let paths = [
        "kicad-config/sym-lib-table",
        "kicad-config/fp-lib-table",
        "global/UniLoader.kicad_sym",
        "project/sym-lib-table",
        "project/fp-lib-table",
        "project/libraries/UniLoader.kicad_sym",
    ];
    for path in paths {
        let path = h.root.join(path);
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("UniLoader_Global", "UniLoader")
            .replace("UniLoader_Local", "UniLoader");
        fs::write(path, text).unwrap();
    }
    let sch = h.root.join("project/board.kicad_sch");
    fs::write(&sch, r#"(kicad_sch (lib_symbols (symbol "UniLoader:Part" (property "Footprint" "UniLoader:Package"))) (symbol (lib_id "UniLoader:Part") (property "Footprint" "UniLoader:Package") (property "Description" "UniLoader:keep")))"#).unwrap();
    let pcb = h.root.join("project/board.kicad_pcb");
    fs::write(
        &pcb,
        r#"(kicad_pcb (footprint "UniLoader:Package" (property "Value" "keep")))"#,
    )
    .unwrap();
    let before = fs::read(&sch).unwrap();
    let args = ["doctor", "--project", h.project.to_str().unwrap()];
    let preview = h.cmd(&args);
    ok(&preview);
    assert!(output(&preview).contains("Would repair"));
    assert_eq!(fs::read(&sch).unwrap(), before);
    // Imports must not silently retain a legacy registration.
    let blocked = h.import(&source, &["--force"]);
    assert!(!blocked.status.success());
    assert!(output(&blocked).contains("doctor"));
    let fixed = h.cmd(&["doctor", "--project", h.project.to_str().unwrap(), "--fix"]);
    ok(&fixed);
    assert!(output(&fixed).contains("Backup:"));
    assert!(
        walkdir::WalkDir::new(h.root.join("xdg-data/uniloader/backups"))
            .into_iter()
            .filter_map(Result::ok)
            .any(|entry| entry.file_type().is_file() && fs::read(entry.path()).unwrap() == before)
    );
    assert!(
        fs::read_to_string(&sch)
            .unwrap()
            .contains("UniLoader_Local:Part")
    );
    assert!(fs::read_to_string(&sch).unwrap().contains("UniLoader:keep"));
    assert!(
        fs::read_to_string(&pcb)
            .unwrap()
            .contains("UniLoader_Local:Package")
    );
    assert_eq!(link(symbols(&h.library())[0]), "UniLoader_Local:Package");
    let global =
        parse(&fs::read_to_string(h.root.join("global/UniLoader.kicad_sym")).unwrap()).unwrap();
    assert_eq!(link(symbols(&global)[0]), "UniLoader_Global:Package");
    let again = h.cmd(&["doctor", "--project", h.project.to_str().unwrap(), "--fix"]);
    ok(&again);
    assert!(output(&again).contains("No nickname repairs needed"));
    ok(&h.import(&source, &["--force"]));
}

#[test]
fn doctor_refuses_occupied_nickname_before_writing_anything() {
    let h = Harness::new("separate");
    fs::create_dir_all(h.root.join("kicad-config")).unwrap();
    let table = h.root.join("kicad-config/fp-lib-table");
    let text = format!(
        r#"(fp_lib_table (lib (name "UniLoader") (type "KiCad") (uri "{}")) (lib (name "UniLoader_Global") (type "KiCad") (uri "/unrelated.pretty") (options "keep")))"#,
        h.root.join("global/UniLoader.pretty").display()
    );
    fs::write(&table, &text).unwrap();
    let result = h.cmd(&["doctor", "--global", "--fix"]);
    assert!(!result.status.success());
    assert!(output(&result).contains("occupied"));
    assert_eq!(fs::read_to_string(table).unwrap(), text);
}

#[test]
fn doctor_updates_global_references_in_projects_after_global_migration() {
    let h = Harness::new("separate");
    let source = h.bundle(
        "source",
        &[
            ("a.kicad_sym", symbol("Part", "Package")),
            ("Package.kicad_mod", footprint("Package", 1.0, "")),
        ],
    );
    ok(&h.cmd(&["import", source.to_str().unwrap(), "--global"]));
    for path in [
        "kicad-config/fp-lib-table",
        "kicad-config/sym-lib-table",
        "global/UniLoader.kicad_sym",
    ] {
        let path = h.root.join(path);
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace("UniLoader_Global", "UniLoader_2"),
        )
        .unwrap();
    }
    ok(&h.cmd(&["doctor", "--global", "--fix"]));
    let sch = h.root.join("project/board.kicad_sch");
    let text = r#"(kicad_sch (symbol (lib_id "UniLoader_2:Part") (property "Footprint" "UniLoader_2:Package")))"#;
    fs::write(&sch, text).unwrap();
    ok(&h.cmd(&["doctor", "--project", h.project.to_str().unwrap(), "--fix"]));
    assert!(
        fs::read_to_string(&sch)
            .unwrap()
            .contains("UniLoader_Global:Part")
    );
    // An unrelated local registration must retain its existing resolution.
    fs::write(&sch, text).unwrap();
    fs::write(
        h.root.join("project/sym-lib-table"),
        r#"(sym_lib_table (lib (name "UniLoader_2") (type "KiCad") (uri "/unrelated.kicad_sym")))"#,
    )
    .unwrap();
    ok(&h.cmd(&["doctor", "--project", h.project.to_str().unwrap(), "--fix"]));
    assert!(
        fs::read_to_string(&sch)
            .unwrap()
            .contains("\"UniLoader_2:Part\"")
    );
}
