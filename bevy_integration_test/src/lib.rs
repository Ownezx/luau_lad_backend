//! A minimal real Bevy + BMS 0.24 application used to generate a live LAD file
//! for the integration tests.

use std::path::{Path, PathBuf};

use bevy::asset::AssetPlugin;
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::MinimalPlugins;
use bevy_mod_scripting::prelude::*;
use ladfile_builder::plugin::ScriptingFilesGenerationPlugin;

/// World position of an entity.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct Position {
    /// World X position.
    pub x: f64,
    /// World Y position.
    pub y: f64,
    /// World Z position.
    pub z: f64,
}

/// Velocity of an entity, in world units per second.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct Velocity {
    /// Velocity along X.
    pub x: f64,
    /// Velocity along Y.
    pub y: f64,
    /// Velocity along Z.
    pub z: f64,
}

/// Health of an entity.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct Health {
    /// Current health.
    pub current: f64,
    /// Maximum health.
    pub max: f64,
}

/// Behavior stance of an entity.
#[derive(Reflect, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[reflect(Default)]
pub enum Stance {
    /// Standing around.
    #[default]
    Idle,
    /// Attacking on sight.
    Aggressive,
}

/// Build a headless scripting app and write its BMS 0.24 LAD dump.
pub fn generate_lad(out_dir: &Path) -> PathBuf {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin::default())
        .add_plugins(LogPlugin::default())
        .add_plugins(BMSPlugin);

    app.register_type::<Position>()
        .register_type::<Velocity>()
        .register_type::<Health>()
        .register_type::<Stance>();

    // BMS 0.24 no longer exposes custom functions as global functions. The
    // script obtains them from `import_namespace("game")`.
    NamespaceBuilder::<NamespaceMembers>::in_namespace(app.world_mut(), "game")
        .register_documented(
            "info",
            |msg: String| {
                info!("[script] {msg}");
            },
            "Write an informational message to the application log.",
            &["msg"],
        )
        .register_documented(
            "magnitude",
            |x: f64, y: f64, z: f64| (x * x + y * y + z * z).sqrt(),
            "Return the Euclidean magnitude of a 3D vector.",
            &["x", "y", "z"],
        );

    app.add_plugins(ScriptingFilesGenerationPlugin::new(
        true,
        out_dir.to_path_buf(),
        Some(PathBuf::from("bindings.lad.json")),
        "Integration-test scripting API",
        true,
        false,
    ));

    app.finish();
    app.cleanup();
    app.update();

    out_dir.join("bindings.lad.json")
}
