//! Stable text output for metadata queries.

use zub::{Artifact, PlanReport};

pub(crate) fn print_plan(report: &PlanReport) {
    println!("plan {}", report.plan);
    println!("manifest {}", report.manifest);
    println!("checksum {}", report.checksum.as_deref().unwrap_or("-"));
    for output in &report.outputs {
        println!(
            "output {} tree={} builder={}",
            output.name, output.tree, output.builder
        );
    }
    for witness in &report.witnesses {
        println!("witness {} tree={}", witness.builder, witness.tree);
    }
}

pub(crate) fn print_artifact(artifact: &Artifact) {
    match artifact {
        Artifact::Elf(elf) => print_elf(elf),
        Artifact::Interface(interface) => {
            println!("artifact interface");
            println!("schema {}", interface.schema);
            println!("output {}", interface.output);
            println!("interface {}", interface.interface);
            for blob in &interface.elf_blobs {
                println!("elf {blob}");
            }
            for header in &interface.headers {
                println!("header {} blob={}", header.path, header.blob);
            }
        }
    }
}

fn print_elf(elf: &zub::ElfArtifact) {
    println!("artifact elf");
    println!("schema {}", elf.schema);
    println!("blob {}", elf.blob);
    println!(
        "soname {}",
        elf.soname
            .as_deref()
            .map(escaped)
            .unwrap_or_else(|| "-".to_owned())
    );
    print_values("needed", &elf.needed);
    print_values("rpath", &elf.rpath);
    print_values("runpath", &elf.runpath);
    for import in &elf.imports {
        println!(
            "import {} version={} library={} weak={}",
            escaped(&import.name),
            optional(import.version.as_deref()),
            escaped(&import.library),
            import.weak
        );
    }
    for export in &elf.exports {
        println!(
            "export {} version={} binding={} hidden={}",
            escaped(&export.name),
            optional(export.version.as_deref()),
            export.binding,
            export.version_hidden
        );
    }
}

fn print_values(label: &str, values: &[Vec<u8>]) {
    for value in values {
        println!("{label} {}", escaped(value));
    }
}

fn optional(value: Option<&[u8]>) -> String {
    value.map(escaped).unwrap_or_else(|| "-".to_owned())
}

fn escaped(value: &[u8]) -> String {
    value
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect()
}
