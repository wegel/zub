mod maintenance;
mod query;
mod store;
mod transfer;

use super::{resolve_repo_path, Cli, Commands};

pub(super) fn run(cli: Cli) -> zub::Result<()> {
    let repo_path = resolve_repo_path(cli.repo);
    match group(&cli.command) {
        Group::Store => store::run(cli.command, &repo_path),
        Group::Maintenance => maintenance::run(cli.command, &repo_path),
        Group::Query => query::run(cli.command, &repo_path),
        Group::Transfer => transfer::run(cli.command, &repo_path),
    }
}

#[derive(Clone, Copy)]
enum Group {
    Store,
    Maintenance,
    Query,
    Transfer,
}

fn group(command: &Commands) -> Group {
    match command {
        Commands::Init { .. }
        | Commands::Commit { .. }
        | Commands::Checkout { .. }
        | Commands::Log { .. }
        | Commands::LsTree { .. }
        | Commands::Diff { .. }
        | Commands::Union { .. }
        | Commands::UnionCheckout { .. } => Group::Store,
        Commands::Fsck { .. }
        | Commands::Gc { .. }
        | Commands::Stats
        | Commands::Du { .. }
        | Commands::TruncateHistory { .. }
        | Commands::Remap { .. } => Group::Maintenance,
        Commands::Push { .. } | Commands::Pull { .. } | Commands::Remote { .. } => Group::Transfer,
        Commands::Refs { .. }
        | Commands::ShowRef { .. }
        | Commands::Plan { .. }
        | Commands::Artifact { .. }
        | Commands::DeleteRef { .. }
        | Commands::DeleteRefs { .. }
        | Commands::DeleteArtifacts { .. }
        | Commands::CatFile { .. }
        | Commands::RevParse { .. }
        | Commands::Show { .. } => Group::Query,
    }
}
