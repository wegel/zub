//! ELF metadata uses real dynamic-linker tables from a compiled fixture.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use zub::ops::commit;
use zub::{
    delete_artifact_ref, parse_elf, read_commit, read_named_artifact, read_tree, write_ref,
    Artifact, Error, Hash, Repo,
};

#[test]
fn elf_metadata_preserves_loader_paths_and_versioned_symbols() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path();
    fs::write(
        root.join("library.c"),
        b"int ep006_export(void) { return 6; }\n",
    )
    .expect("write library source");
    fs::write(
        root.join("versions.map"),
        b"EP006_1 { global: ep006_export; local: *; };\n",
    )
    .expect("write version map");
    compile_library(root);
    symlink("libep006.so.1", root.join("libep006.so")).expect("link development library");

    fs::write(
        root.join("consumer.c"),
        b"extern int ep006_export(void);\nint main(void) { return ep006_export() == 6 ? 0 : 1; }\n",
    )
    .expect("write consumer source");
    compile_consumer(root);

    let library = parse(root.join("libep006.so.1"));
    assert_eq!(library.soname.as_deref(), Some(b"libep006.so.1".as_slice()));
    assert_eq!(library.rpath, [b"/ep006/rpath".to_vec()]);
    assert!(library.runpath.is_empty());
    let export = library
        .exports
        .iter()
        .find(|symbol| symbol.name == b"ep006_export")
        .expect("versioned library export");
    assert_eq!(export.version.as_deref(), Some(b"EP006_1".as_slice()));
    assert_eq!(export.binding, object::elf::STB_GLOBAL.0);

    let consumer = parse(root.join("consumer"));
    assert!(consumer.needed.iter().any(|name| name == b"libep006.so.1"));
    assert!(consumer.rpath.is_empty());
    assert_eq!(consumer.runpath, [b"/ep006/runpath".to_vec()]);
    let import = consumer
        .imports
        .iter()
        .find(|symbol| symbol.name == b"ep006_export")
        .expect("versioned consumer import");
    assert_eq!(import.library, b"libep006.so.1");
    assert_eq!(import.version.as_deref(), Some(b"EP006_1".as_slice()));

    let status = Command::new(root.join("consumer"))
        .env("LD_LIBRARY_PATH", root)
        .status()
        .expect("run compiled consumer");
    assert!(status.success());
}

#[test]
fn malformed_elf_magic_is_an_error() {
    let hash = Hash::from_bytes([0x44; 32]);
    let error = parse_elf(hash, b"\x7fELFbroken").expect_err("reject malformed ELF");
    assert!(matches!(error, Error::ElfMetadata { hash: actual, .. } if actual == hash));
}

#[test]
fn publishing_a_ref_derives_and_repairs_elf_metadata() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path();
    fs::write(
        root.join("library.c"),
        b"int ep006_export(void) { return 6; }\n",
    )
    .expect("write library source");
    fs::write(
        root.join("versions.map"),
        b"EP006_1 { global: ep006_export; local: *; };\n",
    )
    .expect("write version map");
    compile_library(root);
    let source = root.join("source");
    fs::create_dir(&source).expect("create source");
    fs::copy(root.join("libep006.so.1"), source.join("libep006.so.1"))
        .expect("copy library into source");
    let repo = Repo::init(&root.join("repo")).expect("initialize repository");

    let commit = commit(&repo, &source, "library", None, None).expect("commit library");
    let blob = root_blob(&repo, commit, "libep006.so.1");
    let key = format!("elf/{blob}");
    assert!(matches!(
        read_named_artifact(&repo, &key).expect("read derived artifact"),
        Artifact::Elf(artifact) if artifact.blob == blob
    ));

    delete_artifact_ref(&repo, &key).expect("delete derived ref");
    write_ref(&repo, "library-copy", &commit).expect("publish existing commit");
    assert!(matches!(
        read_named_artifact(&repo, &key).expect("read repaired artifact"),
        Artifact::Elf(artifact) if artifact.blob == blob
    ));
}

fn compile_library(root: &Path) {
    run_cc(
        root,
        &[
            "-shared",
            "-fPIC",
            "-Wl,-soname,libep006.so.1",
            "-Wl,--version-script=versions.map",
            "-Wl,--disable-new-dtags,-rpath,/ep006/rpath",
            "-o",
            "libep006.so.1",
            "library.c",
        ],
    );
}

fn compile_consumer(root: &Path) {
    run_cc(
        root,
        &[
            "consumer.c",
            "-L.",
            "-Wl,-rpath,/ep006/runpath",
            "-lep006",
            "-o",
            "consumer",
        ],
    );
}

fn run_cc(root: &Path, arguments: &[&str]) {
    let output = Command::new("cc")
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("run system C compiler");
    assert!(
        output.status.success(),
        "cc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse(path: impl AsRef<Path>) -> zub::ElfArtifact {
    let bytes = fs::read(path).expect("read ELF fixture");
    let hash = Hash::from_bytes(*blake3::hash(&bytes).as_bytes());
    parse_elf(hash, &bytes)
        .expect("parse ELF")
        .expect("ELF artifact")
}

fn root_blob(repo: &Repo, commit: Hash, name: &str) -> Hash {
    let tree = read_commit(repo, &commit).expect("read commit").tree;
    let tree = read_tree(repo, &tree).expect("read root tree");
    match &tree.get(name).expect("root entry").kind {
        zub::EntryKind::Regular { hash, .. } => *hash,
        kind => panic!("expected regular file, found {}", kind.type_name()),
    }
}
