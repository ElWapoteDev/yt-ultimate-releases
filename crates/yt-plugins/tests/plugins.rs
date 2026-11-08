use serde_json::{json, Value};
use std::{fs, path::Path};
use tempfile::TempDir;
use yt_plugins::{PluginError, PluginHost, EXAMPLE_PLUGIN_ID};

const ECHO: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "ytu_alloc") (param i32) (result i32) i32.const 4096)
  (func (export "ytu_command") (param i32 i32) (result i64)
    local.get 0 i64.extend_i32_u i64.const 32 i64.shl
    local.get 1 i64.extend_i32_u i64.or))"#;

fn manifest(id: &str) -> Value {
    json!({"manifest_version":2,"api_version":2,"id":id,"name":"Test plugin","version":"1.0.0","module":"plugin.wasm","commands":[{"id":"echo","label":"Echo"}],"config_schema":{"type":"object","properties":{"label":{"type":"string","minLength":1,"maxLength":20}},"required":["label"],"additionalProperties":false},"default_config":{"label":"initial"}})
}

fn install(root: &Path, id: &str, code: &str) {
    let directory = root.join(id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest(id)).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("plugin.wasm"), wat::parse_str(code).unwrap()).unwrap();
}

#[test]
fn plugins_require_explicit_enablement_and_persist_config_and_opt_in() {
    let temp = TempDir::new().unwrap();
    install(temp.path(), "dev.test.echo", ECHO);
    let host = PluginHost::open(temp.path()).unwrap();
    let info = &host.plugins().unwrap()[0];
    assert!(!info.enabled);
    assert!(info.compatible);
    assert!(matches!(
        host.execute(&info.id, "echo", json!({})),
        Err(PluginError::Disabled(_))
    ));
    host.set_enabled(&info.id, true).unwrap();
    host.set_config(&info.id, json!({"label":"updated"}))
        .unwrap();
    assert_eq!(
        host.execute(&info.id, "echo", json!({"value":42})).unwrap(),
        json!({"command":"echo","arguments":{"value":42},"config":{"label":"updated"}})
    );
    drop(host);
    let reopened = PluginHost::open(temp.path()).unwrap();
    assert!(reopened.plugins().unwrap()[0].enabled);
    assert_eq!(
        reopened.config("dev.test.echo").unwrap(),
        json!({"label":"updated"})
    );
    reopened.set_enabled("dev.test.echo", false).unwrap();
    assert!(matches!(
        reopened.execute("dev.test.echo", "echo", json!({})),
        Err(PluginError::Disabled(_))
    ));
}

#[test]
fn changed_module_or_manifest_requires_renewed_enablement() {
    let temp = TempDir::new().unwrap();
    let id = "dev.test.changed";
    install(temp.path(), id, ECHO);
    let host = PluginHost::open(temp.path()).unwrap();
    host.set_enabled(id, true).unwrap();
    let mut changed = manifest(id);
    changed["description"] = json!("Changed manifest");
    fs::write(
        temp.path().join(id).join("plugin.json"),
        changed.to_string(),
    )
    .unwrap();
    let info = host.plugins().unwrap().remove(0);
    assert!(!info.enabled);
    assert_eq!(info.status, "changed");
    assert!(matches!(
        host.execute(id, "echo", json!({})),
        Err(PluginError::Disabled(_))
    ));
    host.set_enabled(id, true).unwrap();
    let altered = ECHO.replace("i32.const 4096", "i32.const 8192");
    fs::write(
        temp.path().join(id).join("plugin.wasm"),
        wat::parse_str(&altered).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        host.execute(id, "echo", json!({})),
        Err(PluginError::Disabled(_))
    ));
}

#[test]
fn validates_configuration_arguments_and_declared_commands() {
    let temp = TempDir::new().unwrap();
    let id = "dev.test.schema";
    install(temp.path(), id, ECHO);
    let mut manifest = manifest(id);
    manifest["commands"][0]["arguments_schema"] = json!({"type":"object","properties":{"count":{"type":"integer","minimum":1,"maximum":3}},"required":["count"],"additionalProperties":false});
    fs::write(
        temp.path().join(id).join("plugin.json"),
        manifest.to_string(),
    )
    .unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    host.set_enabled(id, true).unwrap();
    for config in [
        json!({}),
        json!({"label":false}),
        json!({"label":""}),
        json!({"label":"ok","secret":true}),
    ] {
        assert!(host.set_config(id, config).is_err());
    }
    assert_eq!(host.config(id).unwrap(), json!({"label":"initial"}));
    assert!(host.execute(id, "missing", json!({})).is_err());
    assert!(host.execute(id, "echo", json!({"count":0})).is_err());
    assert!(host.execute(id, "echo", json!({"count":2})).is_ok());
}

#[test]
fn bounds_infinite_execution_memory_growth_and_output_pointers() {
    let temp = TempDir::new().unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    let prefix="(module (memory (export \"memory\") 1) (func (export \"ytu_alloc\") (param i32) (result i32) i32.const 4096) ";
    for (id,body) in [
        ("dev.test.loop","(func (export \"ytu_command\") (param i32 i32) (result i64) (loop br 0) i64.const 0))"),
        ("dev.test.memory","(func (export \"ytu_command\") (param i32 i32) (result i64) i32.const 1024 memory.grow drop i64.const 0))"),
        ("dev.test.pointer","(func (export \"ytu_command\") (param i32 i32) (result i64) i64.const -1))"),
    ] {
        install(temp.path(),id,&format!("{prefix}{body}"));host.set_enabled(id,true).unwrap();
        assert!(host.execute(id,"echo",json!({})).is_err(),"guest {id} should be stopped");
    }
    install(temp.path(), "dev.test.input", ECHO);
    host.set_enabled("dev.test.input", true).unwrap();
    assert!(host
        .execute(
            "dev.test.input",
            "echo",
            json!({"large":"x".repeat(70*1024)})
        )
        .is_err());
}

#[test]
fn rejects_host_imports_start_functions_and_oversized_initial_memory() {
    let temp = TempDir::new().unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    let imported = ECHO.replacen(
        "(module",
        "(module (import \"wasi_snapshot_preview1\" \"proc_exit\" (func (param i32)))",
        1,
    );
    let started = ECHO.replacen("(module", "(module (func $start) (start $start)", 1);
    let oversized = ECHO.replace(
        "(memory (export \"memory\") 1)",
        "(memory (export \"memory\") 2048)",
    );
    for (id, code) in [
        ("dev.test.import", imported),
        ("dev.test.start", started),
        ("dev.test.big", oversized),
    ] {
        install(temp.path(), id, &code);
        assert!(
            host.set_enabled(id, true).is_err(),
            "guest {id} must be rejected"
        );
        assert!(
            !host
                .plugins()
                .unwrap()
                .iter()
                .find(|info| info.id == id)
                .unwrap()
                .enabled
        );
    }
}

#[test]
fn preserves_legacy_python_and_reports_bad_manifests_without_hiding_good_plugins() {
    let temp = TempDir::new().unwrap();
    install(temp.path(), "dev.test.good", ECHO);
    let legacy = temp.path().join("dev.test.legacy");
    fs::create_dir(&legacy).unwrap();
    let python = b"raise RuntimeError('must never execute this Python plugin')\n";
    fs::write(legacy.join("plugin.py"), python).unwrap();
    fs::write(legacy.join("plugin.json"),json!({"api_version":1,"id":"dev.test.legacy","name":"Legacy","version":"1.0.0","entrypoint":"plugin.py:setup"}).to_string()).unwrap();
    let broken = temp.path().join("dev.test.broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join("plugin.json"), "{invalid").unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    let plugins = host.plugins().unwrap();
    assert_eq!(plugins.len(), 3);
    assert_eq!(
        plugins
            .iter()
            .find(|info| info.id == "dev.test.legacy")
            .unwrap()
            .status,
        "legacy"
    );
    assert!(
        plugins
            .iter()
            .find(|info| info.id == "dev.test.good")
            .unwrap()
            .compatible
    );
    assert!(host.set_enabled("dev.test.legacy", true).is_err());
    assert_eq!(fs::read(legacy.join("plugin.py")).unwrap(), python);
}

#[test]
fn refuses_path_traversal_and_unsupported_schema_keywords() {
    let temp = TempDir::new().unwrap();
    let id = "dev.test.path";
    install(temp.path(), id, ECHO);
    let mut manifest = manifest(id);
    manifest["module"] = json!("../outside.wasm");
    fs::write(
        temp.path().join(id).join("plugin.json"),
        manifest.to_string(),
    )
    .unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    assert!(!host.plugins().unwrap()[0].compatible);
    assert!(host.set_enabled("../outside", true).is_err());
    manifest["module"] = json!("plugin.wasm");
    manifest["config_schema"]["$ref"] = json!("https://example.invalid/schema");
    fs::write(
        temp.path().join(id).join("plugin.json"),
        manifest.to_string(),
    )
    .unwrap();
    assert!(!host.plugins().unwrap()[0].compatible);
}

#[cfg(unix)]
#[test]
fn refuses_module_symlinks_outside_plugin_directory() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let id = "dev.test.symlink";
    install(temp.path(), id, ECHO);
    let module = temp.path().join(id).join("plugin.wasm");
    let target = outside.path().join("outside.wasm");
    fs::rename(&module, &target).unwrap();
    std::os::unix::fs::symlink(&target, &module).unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    assert!(host.set_enabled(id, true).is_err());
    assert!(!host.plugins().unwrap()[0].compatible);
}

#[test]
fn bundled_rust_guest_executes_and_install_never_overwrites_existing_files() {
    let temp = TempDir::new().unwrap();
    let host = PluginHost::open(temp.path()).unwrap();
    assert!(host.install_example().unwrap());
    assert!(!host.install_example().unwrap());
    assert!(!host.plugins().unwrap()[0].enabled);
    host.set_enabled(EXAMPLE_PLUGIN_ID, true).unwrap();
    assert_eq!(
        host.execute(EXAMPLE_PLUGIN_ID, "greet", json!({"name":"Alex"}))
            .unwrap(),
        json!({"message":"Hola, Alex!"})
    );
    host.set_config(
        EXAMPLE_PLUGIN_ID,
        json!({"greeting":"Bienvenido","uppercase":true}),
    )
    .unwrap();
    assert_eq!(
        host.execute(EXAMPLE_PLUGIN_ID, "greet", json!({})).unwrap(),
        json!({"message":"BIENVENIDO, MUNDO!"})
    );
    let manifest = temp.path().join(EXAMPLE_PLUGIN_ID).join("plugin.json");
    fs::write(&manifest, b"user edits").unwrap();
    assert!(!host.install_example().unwrap());
    assert_eq!(fs::read(manifest).unwrap(), b"user edits");
}
