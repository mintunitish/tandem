//! List the bundled skill catalog, or materialize a preset into a directory.
//!
//! ```text
//! cargo run -p tandem-skills --example skills_demo
//! cargo run -p tandem-skills --example skills_demo -- --materialize /tmp/wt --preset lazy [--targets claude,agents,kiro] [--symlink]
//! cargo run -p tandem-skills --example skills_demo -- --clean /tmp/wt
//! ```
use std::path::PathBuf;
use tandem_skills::{AgentTarget, MaterializeOptions, Preset, catalog, clean, materialize_with};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();

    if let Some(dir) = val("--clean") {
        clean(&PathBuf::from(&dir))?;
        println!("cleaned {dir}");
        return Ok(());
    }

    if let Some(dir) = val("--materialize") {
        let preset = val("--preset").unwrap_or_else(|| "lazy".into());
        let preset = Preset::from_name(&preset).ok_or_else(|| {
            anyhow::anyhow!("unknown preset `{preset}` (lazy, terse, disciplined, ship, tandem)")
        })?;
        let targets: Vec<AgentTarget> = match val("--targets") {
            Some(list) => list
                .split(',')
                .map(|t| AgentTarget::from_name(t).ok_or_else(|| anyhow::anyhow!("unknown target `{t}`")))
                .collect::<Result<_, _>>()?,
            None => vec![AgentTarget::ClaudeCode, AgentTarget::Agents],
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir)?;
        let opts = MaterializeOptions { symlink: args.iter().any(|a| a == "--symlink") };
        let report = materialize_with(&dir, &preset.skills(), &targets, opts)?;
        println!("materialized preset `{}` into {}", preset.name(), dir.display());
        for d in &report.dirs {
            println!("  {}", d.display());
        }
        for f in &report.index_files {
            println!("  {} (index block)", f.display());
        }
        for (p, why) in &report.skipped {
            println!("  skipped {}: {why}", p.display());
        }
        return Ok(());
    }

    for pack in catalog() {
        println!("{} [{}] {} ({} skills)", pack.id, pack.license, pack.name, pack.skills.len());
        for s in &pack.skills {
            let desc: String = s.description.split_whitespace().collect::<Vec<_>>().join(" ");
            let desc: String = desc.chars().take(90).collect();
            println!("  {:<34}{}{}", s.id, if s.always_on { "* " } else { "  " }, desc);
        }
    }
    println!("\n(* = always-on mode)  presets: {}", Preset::ALL.map(|p| p.name()).join(", "));
    Ok(())
}
