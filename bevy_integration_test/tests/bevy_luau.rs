//! BMS 0.24 end-to-end integration test.
//!
//! It generates a LAD file from a live Bevy registry, converts it to Luau
//! definitions, and optionally validates those definitions with `luau-lsp`.

use std::path::{Path, PathBuf};
use std::process::Command;

const GOOD_SCRIPT: &str = "\
--!strict
local game = import_namespace(\"game\")
local speed: number = game.magnitude(3, 4, 0)
game.info(\"hero ready\")

local p = (nil :: any) :: Position
local x: number = p.x
local _ = speed + x
";

const BAD_FIELD_SCRIPT: &str = "\
--!strict
local p = (nil :: any) :: Position
local _ = p.acceleration
";

const BAD_ARG_SCRIPT: &str = "\
--!strict
local game = import_namespace(\"game\")
local _ = game.magnitude(\"fast\", 1, 2)
";

#[test]
fn generated_defs_use_bms_024_namespaces_and_typecheck() {
    let dir = scratch_dir("bms024_luau_bevy");
    let lad_path = bevy_integration_test::generate_lad(&dir);
    let lad_src = std::fs::read_to_string(&lad_path).expect("LAD file was generated");
    let lad = ladfile::parse_lad_file(&lad_src).expect("generated LAD parses");

    assert!(lad.version.starts_with("0.24"), "unexpected LAD version: {}", lad.version);
    assert!(lad.namespaces.contains_key("game"), "custom namespace is missing");
    let defs = luau_lad_backend::lad_to_luau(&lad);
    for needle in [
        "declare extern type Position",
        "declare extern type Velocity",
        "declare extern type Health",
        "import_namespace(name: \"game\")",
        "magnitude: (x: number, y: number, z: number) -> number",
        "info: (msg: string) -> nil",
    ] {
        assert!(defs.contains(needle), "generated defs missing `{needle}`:\n{defs}");
    }

    let defs_path = dir.join("api.d.luau");
    std::fs::write(&defs_path, &defs).unwrap();

    let Some(lsp) = find_luau_lsp() else {
        eprintln!("SKIP: luau-lsp not found; generation and schema assertions passed.");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    let good = analyze(&lsp, &defs_path, &dir, "good.luau", GOOD_SCRIPT);
    assert!(
        good.status.success(),
        "luau-lsp rejected a correct BMS 0.24 script (exit {:?}):\n{}",
        good.status.code(),
        good.combined()
    );

    let bad_field = analyze(&lsp, &defs_path, &dir, "bad_field.luau", BAD_FIELD_SCRIPT);
    assert!(
        !bad_field.status.success(),
        "luau-lsp accepted an invalid reflected field access:\n{}",
        bad_field.combined()
    );

    let bad_arg = analyze(&lsp, &defs_path, &dir, "bad_arg.luau", BAD_ARG_SCRIPT);
    assert!(
        !bad_arg.status.success(),
        "luau-lsp accepted an invalid namespace function argument:\n{}",
        bad_arg.combined()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn analyze(lsp: &Path, defs: &Path, dir: &Path, name: &str, script: &str) -> Output {
    let script_path = dir.join(name);
    std::fs::write(&script_path, script).unwrap();
    let mut command = Command::new(lsp);
    command.arg("analyze");
    for flag in luau_lad_backend::fflags_args().lines() {
        command.arg(flag);
    }
    let out = command
        .arg(format!("--defs={}", defs.display()))
        .arg(&script_path)
        .output()
        .expect("failed to run luau-lsp");
    Output {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

struct Output {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl Output {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn find_luau_lsp() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("LUAU_LSP") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from("luau-lsp"));
    candidates.into_iter().find(|path| {
        Command::new(path)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

fn scratch_dir(prefix: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    dir.push(format!("{prefix}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}
