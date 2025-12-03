# zub

A git-like content-addressed store for filesystem trees.

## what it is

zub stores directory trees as content-addressed objects (blobs, trees, commits) with full metadata preservation: ownership, permissions, xattrs, sparse files, and hardlinks.

Similar to [ostree's bare repo mode](https://ostreedev.github.io/ostree/repo/), blobs are stored uncompressed with metadata applied directly to the files, enabling hardlink-based checkout for zero-copy extraction.

## why

- simpler than ostree
- supports user namespace remapping (rootless containers)
- union merging for layer composition
- efficient deduplication via content-addressing

## build

```
cargo build --release
```

## usage

```sh
# init a repo
zub init /path/to/repo

# optionally, create a .zub symlink to avoid -r on every command
ln -s /path/to/repo .zub

# commit a directory tree
zub commit /some/dir my-ref -m "initial"

# checkout (hardlinks by default)
zub checkout my-ref /target/dir

# view history
zub log my-ref

# diff two refs
zub diff ref-a ref-b

# merge multiple refs (last-wins on conflict)
zub union ref-a ref-b ref-c merged-ref --on-conflict last

# sync between repos
zub push /other/repo my-ref
zub pull user@host:/remote/repo some-ref
```

## commands

| command | description |
|---------|-------------|
| `init` | create a new repository |
| `commit` | snapshot a directory into a ref |
| `checkout` | extract a ref to a directory |
| `log` | show commit history |
| `diff` | compare two refs |
| `ls-tree` | list tree contents |
| `union` | merge multiple refs |
| `push` / `pull` | sync refs between repositories |
| `gc` | garbage collect unreachable objects |
| `fsck` | verify repository integrity |
| `remap` | translate blob ownership across namespaces |
| `stats` / `du` | repository statistics and disk usage |

Run `zub --help` for full command list.

## repository layout

```
repo/
├── config.toml
├── objects/
│   ├── blobs/      # file content (uncompressed, with metadata)
│   ├── trees/      # directory structure (cbor + zstd)
│   └── commits/    # commit metadata (cbor + zstd)
└── refs/
    ├── heads/
    └── tags/
```

## license

MIT
