//! Public metadata commands print stable, complete records.

use std::fs;
use std::process::{Command, Output};

use zub::ops::{commit, commit_tree_with_metadata, fsck};
use zub::{
    read_commit, read_ref, read_tree, rebuild_index, witness_ref, write_named_artifact, Artifact,
    Hash, InterfaceArtifact, InterfaceHeader, Repo, ARTIFACT_SCHEMA,
};

#[test]
fn metadata_query_commands_have_stable_output() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let repo = Repo::init(&temporary.path().join("repo")).expect("initialize repository");
    let source = temporary.path().join("source");
    fs::create_dir(&source).expect("create source");
    fs::write(source.join("header.h"), b"int ep006(void);\n").expect("write source");
    let scratch = commit(&repo, &source, "scratch", None, None).expect("commit source");
    let tree = read_commit(&repo, &scratch).expect("read commit").tree;
    let tree_object = read_tree(&repo, &tree).expect("read tree");
    let blob = *tree_object
        .get("header.h")
        .expect("header entry")
        .kind
        .hash()
        .expect("header blob");
    zub::delete_ref(&repo, "scratch").expect("delete scratch ref");

    let plan = Hash::from_bytes([0x11; 32]);
    let manifest = "blob:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let checksum = "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write_record(
        &repo,
        plan,
        "files",
        tree,
        "builder-one",
        manifest,
        checksum,
    );
    write_record(
        &repo,
        plan,
        "outputs/dev",
        tree,
        "builder-one",
        manifest,
        checksum,
    );
    write_witness(&repo, plan, tree, "builder-one", manifest, checksum);
    write_witness(&repo, plan, tree, "builder-two", manifest, checksum);

    let interface = Hash::from_bytes([0x22; 32]);
    write_named_artifact(
        &repo,
        &format!("interface/{tree}"),
        &Artifact::Interface(InterfaceArtifact {
            schema: ARTIFACT_SCHEMA,
            output: tree,
            interface,
            elf_blobs: vec![blob],
            headers: vec![InterfaceHeader {
                path: "/usr/include/ep006.h".to_owned(),
                blob,
            }],
        }),
    )
    .expect("write interface artifact");

    let plan_output = run(&repo, &["plan", &plan.to_string()]);
    assert_eq!(
        stdout(plan_output),
        format!(
            "plan {plan}\nmanifest {manifest}\nchecksum {checksum}\n\
             output files tree={tree} builder=builder-one\n\
             output outputs/dev tree={tree} builder=builder-one\n\
             witness builder-one tree={tree}\n\
             witness builder-two tree={tree}\n"
        )
    );

    let refs_output = run(&repo, &["refs", "--blob", &blob.to_string()]);
    let mut names = [
        format!("plan/{plan}/files"),
        format!("plan/{plan}/outputs/dev"),
        witness_ref(plan, "builder-one"),
        witness_ref(plan, "builder-two"),
    ];
    names.sort();
    let expected = names
        .iter()
        .map(|name| {
            format!(
                "{} {name}\n",
                read_ref(&repo, name).expect("read record ref")
            )
        })
        .collect::<String>();
    assert_eq!(stdout(refs_output), expected);

    let artifact_output = run(&repo, &["artifact", &format!("interface/{tree}")]);
    assert_eq!(
        stdout(artifact_output),
        format!(
            "artifact interface\nschema 1\noutput {tree}\ninterface {interface}\n\
             elf {blob}\nheader /usr/include/ep006.h blob={blob}\n"
        )
    );

    fs::remove_dir_all(repo.index_path()).expect("remove temporary derived index");
    let report = fsck(&repo).expect("inspect repository before reindex");
    let markers = rebuild_index(&repo).expect("count expected index markers");
    fs::remove_dir_all(repo.index_path()).expect("remove temporary derived index again");
    let fsck_output = run(&repo, &["fsck", "--reindex"]);
    assert_eq!(
        stdout(fsck_output),
        format!(
            "objects checked: {}\n\ndangling objects: {}\n\nrepository is healthy\n\
             rebuilt reverse index: {markers} blob/tree links\n",
            report.objects_checked,
            report.dangling_objects.len()
        )
    );
}

fn write_record(
    repo: &Repo,
    plan: Hash,
    suffix: &str,
    tree: Hash,
    builder: &str,
    manifest: &str,
    checksum: &str,
) {
    let reference = format!("plan/{plan}/{suffix}");
    write_commit(repo, &reference, plan, tree, builder, manifest, checksum);
}

fn write_witness(
    repo: &Repo,
    plan: Hash,
    tree: Hash,
    builder: &str,
    manifest: &str,
    checksum: &str,
) {
    write_commit(
        repo,
        &witness_ref(plan, builder),
        plan,
        tree,
        builder,
        manifest,
        checksum,
    );
}

fn write_commit(
    repo: &Repo,
    reference: &str,
    plan: Hash,
    tree: Hash,
    builder: &str,
    manifest: &str,
    checksum: &str,
) {
    let plan = plan.to_string();
    commit_tree_with_metadata(
        repo,
        &tree,
        reference,
        None,
        Some("nex"),
        &[
            ("nex.plan", &plan),
            ("nex.manifest", manifest),
            ("nex.checksum", checksum),
            ("nex.builder", builder),
        ],
    )
    .expect("write record commit");
}

fn run(repo: &Repo, arguments: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_zub"))
        .arg("--repo")
        .arg(repo.path())
        .args(arguments)
        .output()
        .expect("run zub command");
    assert!(
        output.status.success(),
        "zub command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: Output) -> String {
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}
