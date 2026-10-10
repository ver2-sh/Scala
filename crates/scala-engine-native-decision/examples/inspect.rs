//! Read-only source binding inspection. Never initialize application state or a runtime.
use scala_core::{ArtifactFormat, DecisionBundle, ModelRegistry};
fn main() {
    let roots: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    if roots.is_empty() {
        eprintln!("usage: inspect <source-bundle-directory>...");
        std::process::exit(2);
    }
    let registry = ModelRegistry::discover(&roots);
    for warning in registry.warnings() {
        eprintln!("{warning}");
    }
    let mut count = 0;
    for artifact in registry
        .artifacts()
        .iter()
        .filter(|a| a.format == ArtifactFormat::DecisionBundle)
    {
        let bundle = DecisionBundle::read(&artifact.path).expect("discovered source bundle");
        println!(
            "{}",
            serde_json::json!({"artifact_id":artifact.id,"path":artifact.path,"backend":bundle.backend,"sources":bundle.sources.iter().map(|(role,s)| (role,serde_json::json!({"repository":s.repository,"revision":s.revision}))).collect::<std::collections::BTreeMap<_,_>>(),"execution_qualified":false})
        );
        count += 1;
    }
    if count == 0 || !registry.warnings().is_empty() {
        std::process::exit(1);
    }
}
