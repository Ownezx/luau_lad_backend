# luau_lad_backend

[![crates.io](https://img.shields.io/crates/v/luau_lad_backend.svg)](https://crates.io/crates/luau_lad_backend)
[![docs.rs](https://img.shields.io/docsrs/luau_lad_backend)](https://docs.rs/luau_lad_backend)
[![CI](https://github.com/jonasdedden/luau_lad_backend/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasdedden/luau_lad_backend/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue)](https://github.com/jonasdedden/luau_lad_backend/blob/main/Cargo.toml)

A native **Luau** (`.d.luau`) definition-file backend for
[`bevy_mod_scripting`](https://github.com/makspll/bevy_mod_scripting) (BMS)
Language Agnostic Definition (LAD) files. The generated definitions let
[`luau-lsp`](https://github.com/JohnnyMorganz/luau-lsp) type-check scripts
against the API exposed by a Rust/Bevy application.

This release targets the BMS 0.24 LAD schema. That schema describes script
namespaces, typed type values, generic parameters, and explicit method
receivers; it is a breaking change from earlier LAD versions.

## Compatibility

| `luau_lad_backend` | `ladfile` / BMS | Bevy (integration test) | MSRV |
| --- | --- | --- | --- |
| 0.3.x | 0.24.x | 0.20.0 | 1.95 |
| 0.2.x | 0.20–0.21 | 0.18–0.19 | 1.95 |
| 0.1.x | 0.17–0.19 | 0.16–0.18 | 1.89 |

The 0.3.x crate pins `ladfile = 0.24.0` because `ladfile` is part of this
crate's public API and the 0.24 format is not compatible with older schemas.
BMS 0.24 uses Bevy 0.20.0; the integration-test workspace pins that exact
Bevy version as well.

## What is generated

- **Reflected types.** Named types in the LAD file become Luau extern types.
  Struct fields keep their declared types; enum struct-variant fields are
  optional and same-named fields are unioned across variants.
- **Namespace imports.** Each LAD namespace becomes a typed Luau table, and
  string-literal overloads of `import_namespace` return the matching table.
  For example, the generated type for `import_namespace("game")` exposes the
  functions and values registered in the `game` namespace.
- **Type values and registrations.** LAD `Type<T>` values use the registered
  runtime registration type parameterized by `T`, intersected with the static
  members table for the reflected type. This preserves BMS 0.24's explicit
  type-value and generic-signature metadata instead of inferring registrations
  from argument positions.
- **Generic templates and methods.** `ladfile.generic_types` becomes generic
  extern type declarations. Generic parameters in function signatures and
  concrete type layouts are retained.
- **Method receivers.** The explicit LAD receiver flag controls whether a
  function is emitted as an instance method and whether its first argument is
  implicit in Luau method-call syntax.
- **Enum metadata.** Enums export a variant-name union
  (`export type StanceVariant = "Idle" | "Aggressive"`) and a static type
  table for variant-specific functions.
- **Constructor payload aliases.** With the default options, named struct
  layouts produce `<Type>Init` aliases based on LAD's default-field metadata.
  Disable them with `Options::init_types = false` or `--no-init-types` to
  reduce output size and analysis time.
- **Documentation.** Type, function, argument, and return documentation is
  emitted as Luau comments. Reserved field names are preserved with quoted
  table keys; names that cannot be used as Luau identifiers are not fabricated
  as renamed function members.

Unsupported or deliberately dynamic shapes fall back to `any`. Positional
fields in tuple structs and tuple variants are currently omitted because the
LAD shape does not provide named Luau table fields for them. Methods recorded
only on a generic type's individual instantiations are not yet projected onto
the generic template; methods attached to concrete LAD type definitions are
emitted. Callback argument signatures are not available from the LAD callback
registry. These limits are preferable to emitting misleading, invalid
declarations.

## Install

```bash
cargo add luau_lad_backend
cargo install luau_lad_backend
```

## Usage

### As a library

```rust
let lad = ladfile::parse_lad_file(json)?;
let defs: String = luau_lad_backend::lad_to_luau(&lad);

// Or omit constructor-payload aliases:
use luau_lad_backend::{lad_to_luau_with, Options};
let defs = lad_to_luau_with(&lad, &Options { init_types: false });
```

### As a BMS generation-pipeline processor

`LuauLadPlugin` implements `ladfile::LadFilePlugin` and can be added to the
LAD generation processor list. When your application imports BMS's generation
settings directly, declare `ladfile_builder` as a direct dependency too (Rust
code cannot import a transitive dependency):

```toml
ladfile_builder = { version = "=0.24.0", default-features = false }
luau_lad_backend = { path = "../luau_lad_backend" }
```

```rust
use ladfile_builder::plugin::LadFileSettings;
use luau_lad_backend::LuauLadPlugin;

let mut settings = LadFileSettings::default();
settings.processors.push(Box::new(LuauLadPlugin {
    // The default output is luau/bindings.d.luau, separate from BMS's LuaLS files.
    fflags_filename: Some("luau/fflags.txt".into()),
    ..Default::default()
}));
```

### As a CLI

```bash
lad-luau bindings.lad.json bindings.d.luau
lad-luau --check bindings.lad.json bindings.d.luau
lad-luau --fflags
lad-luau --no-init-types bindings.lad.json bindings.d.luau
lad-luau --help
```

`--check` regenerates the definitions in memory and compares them with the
file on disk, returning a non-zero exit code when the committed file is stale.
Without an output path, `lad-luau` writes the generated definitions to stdout.

BMS 0.24 moved most script-facing APIs into namespaces. A typical script now
starts with imports such as:

```luau
local bms = import_namespace("bms")
local world = bms.world
local math = import_namespace("math")
local transform = import_namespace("transform")
```

The backend emits typed overloads for namespaces present in the LAD file. To
analyze scripts, point `luau-lsp` at the generated definition file:

```bash
luau-lsp analyze --defs=assets/lua/definitions/luau/bindings.d.luau scripts/level1.luau
```

```jsonc
// .vscode/settings.json — also available as examples/vscode-luau-lsp-settings.json.
// Paths are relative to your workspace root.
{
  "luau-lsp.platform.type": "standard",
  "luau-lsp.types.definitionFiles": [
    "assets/lua/definitions/luau/bindings.d.luau"
  ],
  // BMS also emits LuaLS/EmmyLua `.d.lua` files when its
  // `lua_language_server_files` feature is enabled. They are not Luau definitions.
  "luau-lsp.ignoreGlobs": [
    "**/assets/lua/definitions/*.d.lua",
    "**/assets/lua/definitions/**/*.d.lua"
  ],
  "luau-lsp.fflags.override": {
    "LuauTypeInferIterationLimit": "10000000",
    "LuauTarjanChildLimit": "1000000",
    "LuauTypeCloneIterationLimit": "10000000",
    "LuauSolverConstraintLimit": "100000",
    "LuauTypeInferRecursionLimit": "2000"
  }
}
```

### Important: BMS also writes LuaLS files

When BMS is built with `lua_language_server_files`, its generation pipeline writes
`bindings.d.lua` and one `bindings.<namespace>.d.lua` file per namespace. Those
files use LuaLS/EmmyLua annotations (`---@class`, `---@field`, and so on); they
are **not** Luau definition files and must not be treated as Luau source or passed
to `luau-lsp`. The backend writes its native `bindings.d.luau` to the separate
`luau/` subdirectory by default. Keep the two dialects separate and have
`luau-lsp` ignore the BMS `*.d.lua` files as shown in the settings example. After
changing `ignoreGlobs`, close any already-open generated `.d.lua` tabs and restart
the Luau language server so the workspace index is rebuilt.

If you intentionally use both languages, configure LuaLS separately with a
`.luarc.json` such as:

```json
{
  "$schema": "https://raw.githubusercontent.com/LuaLS/vscode-lua/master/setting/schema.json",
  "workspace.library": ["assets/lua/definitions"],
  "workspace.preloadFileSize": 10000
}
```

The `workspace.library` directory must contain the root and per-namespace
`.d.lua` files, as described in the [BMS editor-support documentation](https://makspll.github.io/bevy_mod_scripting/ScriptTooling/introduction.html).
In the supplied BMS 0.24 output, the root `bindings.d.lua` already declares
`ReflectReference` and `NoFields`; `bindings.math.d.lua`, `bindings.ecs.d.lua`,
and `bindings.bms.d.lua` declare the namespace types referenced by the other
files. Therefore, do not add a duplicate `NoFields` alias. If LuaLS still reports
these names as undefined, its workspace library is not indexing the whole
output directory. The matching config example is `examples/luarc.json`. If the
project is Luau-only, disable the LuaLS extension for this workspace instead.

The five `luau-lsp` inference flags needed by large reflection registries are
included in every generated file's header, exposed as `FFLAGS` and
`fflags_args()`, and available through `lad-luau --fflags` or
`LuauLadPlugin::fflags_filename`.

## Testing

The workspace includes three levels of checks:

1. Unit tests for LAD 0.24 conversion, namespaced imports, generic types,
   constructor aliases, and Luau field-key handling.
2. A `luau-lsp` test using the bundled BMS 0.24 example LAD file. It checks a
   valid namespace/type access and ensures invalid fields and enum variants are
   rejected.
3. A separate, non-published `bevy_integration_test` crate that builds a
   headless Bevy 0.20 + BMS 0.24 application, generates a live LAD file, and
   checks a custom namespace and reflected component types.

`luau-lsp` is an external binary. The analyzer tests run when it is available
through `LUAU_LSP` or `PATH`, and otherwise skip with a notice:

```bash
LUAU_LSP=/path/to/luau-lsp cargo test --workspace
cargo test --workspace
```

## License

Licensed under either of MIT or Apache-2.0 at your option.
