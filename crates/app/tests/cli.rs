//! End-to-end tests of the `212` binary.
//!
//! `real_inference` needs a small GGUF model and llama-server; it is ignored by
//! default. Run it with:
//!   TWO_ONE_TWO_TEST_GGUF=/path/to/Qwen3-0.6B-Q8_0.gguf cargo test -p two-one-two -- --ignored

use std::process::Command;

fn bin(home: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_212"));
    c.env("TWO_ONE_TWO_HOME", home).env_remove("OLLAMA_MODELS").env("OLLAMA_MODELS", home.join("no-ollama"));
    c
}

#[test]
fn version_and_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("212 "));
    let out = bin(tmp.path()).arg("paths").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("config.toml"));
}

#[test]
fn first_run_writes_a_valid_config_and_lists_no_models() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).args(["models", "list"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("No models installed"));
    let cfg = tmp.path().join("config/config.toml");
    let loaded = config::Config::load(&cfg).unwrap();
    assert!(loaded.warnings.is_empty());
}

#[test]
fn importing_a_non_model_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("fake.gguf");
    std::fs::write(&f, b"definitely not gguf data....").unwrap();
    let out = bin(tmp.path()).args(["models", "import"]).arg(&f).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a usable GGUF"));
}

#[test]
fn prompt_without_model_explains_what_to_do() {
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).args(["-p", "hello"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("No model"), "{err}");
}

#[test]
#[ignore = "needs TWO_ONE_TWO_TEST_GGUF and llama-server"]
fn real_inference() {
    let gguf = std::env::var("TWO_ONE_TWO_TEST_GGUF").expect("set TWO_ONE_TWO_TEST_GGUF to a small GGUF model");
    let tmp = tempfile::tempdir().unwrap();
    let out = bin(tmp.path()).args(["models", "import", &gguf]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let list = String::from_utf8_lossy(&bin(tmp.path()).args(["models", "list"]).output().unwrap().stdout).into_owned();
    let id = list.split_whitespace().next().unwrap().to_string();
    let out = bin(tmp.path()).args(["--model", &id, "-p", "Reply with the single word: pong /no_think"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(!stdout.trim().is_empty(), "no output; stderr: {stderr}");
    assert!(stderr.contains("tokens"), "{stderr}");
    // The backend must be gone and its pid file removed.
    let run = tmp.path().join("data/run");
    assert_eq!(std::fs::read_dir(run).unwrap().count(), 0, "pid files left behind");
}
