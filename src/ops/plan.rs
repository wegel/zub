//! Inspection and validation of plan-keyed Nex records.

use crate::{list_refs, read_commit, read_ref, Error, Hash, Repo, Result};

const PLAN: &str = "nex.plan";
const CHECKSUM: &str = "nex.checksum";
const MANIFEST: &str = "nex.manifest";
const BUILDER: &str = "nex.builder";

/// One current output ref in a plan record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanOutput {
    /// Ref suffix below `plan/<plan>/`.
    pub name: String,
    /// Current output tree.
    pub tree: Hash,
    /// Builder recorded on the current commit.
    pub builder: String,
}

/// One builder's corroborating witness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanWitness {
    /// Human-readable builder name from commit metadata.
    pub builder: String,
    /// Full package or assembly tree produced by that builder.
    pub tree: Hash,
}

/// Validated current state of one plan ref family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanReport {
    /// Plan identity.
    pub plan: Hash,
    /// Git manifest identity shared by every record commit.
    pub manifest: String,
    /// Sealed checksum, or `None` for a bootstrap plan.
    pub checksum: Option<String>,
    /// Current output refs, sorted by suffix.
    pub outputs: Vec<PlanOutput>,
    /// Builder witnesses, sorted by builder name.
    pub witnesses: Vec<PlanWitness>,
}

/// Inspect and validate every current ref for one plan.
pub fn inspect_plan(repo: &Repo, plan: Hash) -> Result<PlanReport> {
    let prefix = format!("plan/{plan}/");
    let refs = list_refs(repo)?
        .into_iter()
        .filter(|name| name.starts_with(&prefix))
        .collect::<Vec<_>>();
    if refs.is_empty() {
        return Err(Error::PlanNotFound(plan));
    }

    let mut state = PlanState::new(plan, prefix);
    for reference in refs {
        state.add(repo, reference)?;
    }
    state.finish()
}

/// Return the safe ref component for one readable builder name.
pub fn builder_key(builder: &str) -> String {
    blake3::hash(builder.as_bytes()).to_hex().to_string()
}

/// Return the normal ref used for one plan witness.
pub fn witness_ref(plan: Hash, builder: &str) -> String {
    format!("plan/{plan}/witness/{}", builder_key(builder))
}

struct PlanState {
    plan: Hash,
    prefix: String,
    manifest: Option<String>,
    checksum: Option<Option<String>>,
    full_tree: Option<Hash>,
    outputs: Vec<PlanOutput>,
    witnesses: Vec<PlanWitness>,
}

impl PlanState {
    fn new(plan: Hash, prefix: String) -> Self {
        Self {
            plan,
            prefix,
            manifest: None,
            checksum: None,
            full_tree: None,
            outputs: Vec::new(),
            witnesses: Vec::new(),
        }
    }

    fn add(&mut self, repo: &Repo, reference: String) -> Result<()> {
        let hash = read_ref(repo, &reference)?;
        crate::verify_commit(repo, &hash)?;
        let commit = read_commit(repo, &hash)?;
        let metadata = RecordMetadata::read(self.plan, &reference, &commit)?;
        self.merge_metadata(&reference, &metadata)?;
        let suffix = reference
            .strip_prefix(&self.prefix)
            .expect("selected plan prefix");
        if let Some(key) = suffix.strip_prefix("witness/") {
            self.add_witness(key, commit.tree, metadata)
        } else {
            self.add_output(suffix, commit.tree, metadata)
        }
    }

    fn merge_metadata(&mut self, reference: &str, metadata: &RecordMetadata) -> Result<()> {
        require_common(
            self.plan,
            reference,
            "manifest",
            &mut self.manifest,
            metadata.manifest.clone(),
        )?;
        require_common(
            self.plan,
            reference,
            "checksum",
            &mut self.checksum,
            metadata.checksum.clone(),
        )?;
        Ok(())
    }

    fn add_output(&mut self, name: &str, tree: Hash, metadata: RecordMetadata) -> Result<()> {
        if matches!(name, "files" | "tree") {
            require_common(
                self.plan,
                name,
                "full output tree",
                &mut self.full_tree,
                tree,
            )?;
        }
        self.outputs.push(PlanOutput {
            name: name.to_owned(),
            tree,
            builder: metadata.builder,
        });
        Ok(())
    }

    fn add_witness(&mut self, key: &str, tree: Hash, metadata: RecordMetadata) -> Result<()> {
        let expected = builder_key(&metadata.builder);
        if key != expected {
            return Err(record_error(
                self.plan,
                format!(
                    "witness builder {} uses key {key}, expected {expected}",
                    metadata.builder
                ),
            ));
        }
        self.witnesses.push(PlanWitness {
            builder: metadata.builder,
            tree,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<PlanReport> {
        let full_tree = self.full_tree.ok_or_else(|| {
            record_error(self.plan, "record has no files or assembly tree".to_owned())
        })?;
        for witness in &self.witnesses {
            if witness.tree != full_tree {
                return Err(record_error(
                    self.plan,
                    format!(
                        "builder {} witnessed tree {}, expected {}",
                        witness.builder, witness.tree, full_tree
                    ),
                ));
            }
        }
        self.outputs
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.witnesses
            .sort_by(|left, right| left.builder.cmp(&right.builder));
        Ok(PlanReport {
            plan: self.plan,
            manifest: self.manifest.expect("record metadata checked"),
            checksum: self.checksum.expect("record metadata checked"),
            outputs: self.outputs,
            witnesses: self.witnesses,
        })
    }
}

struct RecordMetadata {
    manifest: String,
    checksum: Option<String>,
    builder: String,
}

impl RecordMetadata {
    fn read(plan: Hash, reference: &str, commit: &crate::Commit) -> Result<Self> {
        let actual_plan = required(plan, reference, commit, PLAN)?;
        if actual_plan != plan.to_hex() {
            return Err(record_error(
                plan,
                format!("ref {reference} records plan {actual_plan}"),
            ));
        }
        Ok(Self {
            manifest: required(plan, reference, commit, MANIFEST)?.to_owned(),
            checksum: commit.metadata.get(CHECKSUM).cloned(),
            builder: required(plan, reference, commit, BUILDER)?.to_owned(),
        })
    }
}

fn required<'a>(
    plan: Hash,
    reference: &str,
    commit: &'a crate::Commit,
    key: &str,
) -> Result<&'a str> {
    commit
        .metadata
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| record_error(plan, format!("ref {reference} has no {key}")))
}

fn require_common<T: Eq + Clone + std::fmt::Debug>(
    plan: Hash,
    reference: &str,
    field: &str,
    current: &mut Option<T>,
    incoming: T,
) -> Result<()> {
    if current.as_ref().is_some_and(|value| value != &incoming) {
        return Err(record_error(
            plan,
            format!("ref {reference} disagrees on {field}: {incoming:?}"),
        ));
    }
    current.get_or_insert(incoming);
    Ok(())
}

fn record_error(plan: Hash, message: String) -> Error {
    Error::PlanRecord { plan, message }
}
