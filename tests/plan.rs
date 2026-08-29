//! Plan queries validate output records and independent builder witnesses.

use std::fs;

use zub::ops::{commit, commit_tree_with_metadata};
use zub::{builder_key, inspect_plan, read_commit, witness_ref, Error, Hash, Repo};

#[test]
fn plan_report_lists_outputs_and_distinct_builder_witnesses() {
    let fixture = Fixture::new();
    fixture.output("files", fixture.tree, "builder-one");
    fixture.output("outputs/bin", fixture.tree, "builder-one");
    fixture.witness(fixture.tree, "builder-one");
    fixture.witness(fixture.tree, "builder-two");

    let report = inspect_plan(&fixture.repo, fixture.plan).expect("inspect plan");

    assert_eq!(report.manifest, fixture.manifest);
    assert_eq!(report.checksum.as_deref(), Some(fixture.checksum.as_str()));
    assert_eq!(
        report
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["files", "outputs/bin"]
    );
    assert_eq!(
        report
            .witnesses
            .iter()
            .map(|witness| witness.builder.as_str())
            .collect::<Vec<_>>(),
        ["builder-one", "builder-two"]
    );
    assert_eq!(
        witness_ref(fixture.plan, "builder-one"),
        format!(
            "plan/{}/witness/{}",
            fixture.plan,
            builder_key("builder-one")
        )
    );
}

#[test]
fn plan_report_rejects_a_witness_for_another_tree() {
    let fixture = Fixture::new();
    fixture.output("files", fixture.tree, "builder-one");
    fixture.witness(fixture.other_tree(), "builder-two");

    let error = inspect_plan(&fixture.repo, fixture.plan).expect_err("reject witness");

    assert!(matches!(&error, Error::PlanRecord { plan, .. } if *plan == fixture.plan));
    assert!(error.to_string().contains("builder-two witnessed tree"));
}

struct Fixture {
    temporary: tempfile::TempDir,
    repo: Repo,
    plan: Hash,
    tree: Hash,
    manifest: String,
    checksum: String,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = Repo::init(&temporary.path().join("repo")).expect("initialize repository");
        let source = temporary.path().join("source");
        fs::create_dir(&source).expect("create source");
        fs::write(source.join("payload"), b"plan bytes").expect("write payload");
        let commit = commit(&repo, &source, "scratch", None, None).expect("commit source");
        let tree = read_commit(&repo, &commit)
            .expect("read source commit")
            .tree;
        zub::delete_ref(&repo, "scratch").expect("delete scratch ref");
        Self {
            temporary,
            repo,
            plan: Hash::from_bytes([0x11; 32]),
            tree,
            manifest: "blob:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            checksum: "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
        }
    }

    fn output(&self, suffix: &str, tree: Hash, builder: &str) {
        self.commit(&format!("plan/{}/{suffix}", self.plan), tree, builder);
    }

    fn witness(&self, tree: Hash, builder: &str) {
        self.commit(&witness_ref(self.plan, builder), tree, builder);
    }

    fn commit(&self, reference: &str, tree: Hash, builder: &str) {
        let plan = self.plan.to_string();
        commit_tree_with_metadata(
            &self.repo,
            &tree,
            reference,
            None,
            Some("nex"),
            &[
                ("nex.plan", &plan),
                ("nex.manifest", &self.manifest),
                ("nex.checksum", &self.checksum),
                ("nex.builder", builder),
            ],
        )
        .expect("commit record");
    }

    fn other_tree(&self) -> Hash {
        let source = self.temporary.path().join("other");
        fs::create_dir(&source).expect("create other source");
        fs::write(source.join("payload"), b"different plan bytes").expect("write other payload");
        let commit = commit(&self.repo, &source, "other", None, None).expect("commit other tree");
        read_commit(&self.repo, &commit)
            .expect("read other commit")
            .tree
    }
}
