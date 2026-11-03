use serde_json::Value;
use std::{collections::BTreeSet, env, fs, path::Path};

fn read(root: &Path, name: &str) -> Value {
    println!("cargo:rerun-if-changed={}", root.join(name).display());
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}
fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}
fn keep(value: &mut Value, allowed: &BTreeSet<String>) {
    value
        .as_array_mut()
        .unwrap()
        .retain(|v| allowed.contains(v.as_str().unwrap()));
}
fn visit(root: &Path, dir: &Path, files: &mut Vec<String>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit(root, &path, files);
        } else {
            files.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scope = read(&root, "catalog/product-scope.json");
    assert_eq!(scope["schema_version"], "melissa.product-scope/v1");
    let active = strings(&scope["active_modules"]);
    // A future module needs an explicit implementation/review, not just a flag.
    assert_eq!(active, BTreeSet::from(["core".into(), "code".into()]));
    let deferred = strings(&scope["deferred_modules"]);
    assert!(active.is_disjoint(&deferred));
    let mut index = read(&root, "catalog/agent-index.json");
    index["profiles"]
        .as_array_mut()
        .unwrap()
        .retain(|p| active.contains(p["module"].as_str().unwrap()));
    let profile_ids: BTreeSet<String> = index["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    let mut projected = std::collections::BTreeMap::<String, Value>::new();
    let mut workers = BTreeSet::new();
    for entry in index["profiles"].as_array().unwrap() {
        let name = entry["path"].as_str().unwrap();
        let mut p = read(&root, name);
        assert!(active.contains(p["module"].as_str().unwrap()));
        assert!(profile_ids.contains(p["id"].as_str().unwrap()));
        p["handoffs"]
            .as_array_mut()
            .unwrap()
            .retain(|h| profile_ids.contains(h["profile_id"].as_str().unwrap()));
        workers.extend(strings(&p["worker_types"]));
        projected.insert(name.into(), p);
    }
    for id in &workers {
        let name = format!("catalog/workers/{id}.json");
        let mut w = read(&root, &name);
        keep(&mut w["modules"], &active);
        projected.insert(name, w);
    }
    let mut tools = read(&root, "catalog/tools.json");
    tools["tools"]
        .as_array_mut()
        .unwrap()
        .retain(|t| active.contains(t["owner_module"].as_str().unwrap()));
    for t in tools["tools"].as_array_mut().unwrap() {
        keep(&mut t["referenced_by"], &profile_ids);
        keep(&mut t["worker_candidates"], &workers);
    }
    let mut modules = read(&root, "catalog/modules.json");
    modules["modules"]
        .as_array_mut()
        .unwrap()
        .retain(|m| active.contains(m["id"].as_str().unwrap()));
    projected.insert("catalog/agent-index.json".into(), index);
    projected.insert("catalog/tools.json".into(), tools);
    projected.insert("catalog/modules.json".into(), modules);
    let mut files = vec![];
    visit(&root, &root.join("skills/code"), &mut files);
    files.extend(["catalog/tool-aliases.json", "catalog/runtime-policy.json"].map(str::to_owned));
    files.sort();
    let mut generated = String::from("pub const FILES: &[(&str, &[u8])] = &[\n");
    let out = std::path::PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (name, value) in projected {
        let path = out.join("product").join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        generated.push_str(&format!("({name:?}, include_bytes!({path:?})),\n"));
    }
    for name in files {
        println!("cargo:rerun-if-changed={}", root.join(&name).display());
        generated.push_str(&format!(
            "({name:?}, include_bytes!({:?})),\n",
            root.join(&name)
        ));
    }
    generated.push_str("];\n");
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("resources.rs"),
        generated,
    )
    .unwrap();
}
