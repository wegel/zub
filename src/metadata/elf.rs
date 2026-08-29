//! Dynamic-linking facts derived from ELF blobs.

use object::read::{ExportFlags, ImportFlags};
use object::{elf, NameOrOrdinal, Object};

use crate::types::{ElfArtifact, ElfExport, ElfImport, ARTIFACT_SCHEMA};
use crate::{Error, Hash, Result};

/// Parse one ELF blob, or return `None` when the bytes are not ELF-marked.
pub fn parse_elf(blob: Hash, bytes: &[u8]) -> Result<Option<ElfArtifact>> {
    if !bytes.starts_with(b"\x7fELF") {
        return Ok(None);
    }
    let file = object::File::parse(bytes).map_err(|error| invalid(blob, error))?;
    let dynamic = dynamic_strings(blob, &file)?;
    let imports = imports(blob, &file)?;
    let exports = exports(blob, &file)?;
    Ok(Some(ElfArtifact {
        schema: ARTIFACT_SCHEMA,
        blob,
        soname: dynamic.soname,
        needed: dynamic.needed,
        rpath: dynamic.rpath,
        runpath: dynamic.runpath,
        imports,
        exports,
    }))
}

#[derive(Default)]
struct DynamicStrings {
    soname: Option<Vec<u8>>,
    needed: Vec<Vec<u8>>,
    rpath: Vec<Vec<u8>>,
    runpath: Vec<Vec<u8>>,
}

fn dynamic_strings(blob: Hash, file: &object::File<'_>) -> Result<DynamicStrings> {
    match file {
        object::File::Elf32(file) => collect_dynamic(blob, file.elf_dynamic_table()),
        object::File::Elf64(file) => collect_dynamic(blob, file.elf_dynamic_table()),
        _ => Err(Error::ElfMetadata {
            hash: blob,
            message: "ELF magic resolved to another object format".to_owned(),
        }),
    }
}

fn collect_dynamic<Elf>(
    blob: Hash,
    table: object::read::Result<object::read::elf::DynamicTable<'_, Elf>>,
) -> Result<DynamicStrings>
where
    Elf: object::read::elf::FileHeader,
{
    let table = table.map_err(|error| invalid(blob, error))?;
    let mut strings = DynamicStrings::default();
    for entry in table.iter() {
        let target = match entry.tag {
            elf::DT_SONAME => Some(&mut strings.soname),
            elf::DT_NEEDED => {
                strings.needed.push(dynamic_string(blob, &table, entry)?);
                None
            }
            elf::DT_RPATH => {
                strings.rpath.push(dynamic_string(blob, &table, entry)?);
                None
            }
            elf::DT_RUNPATH => {
                strings.runpath.push(dynamic_string(blob, &table, entry)?);
                None
            }
            _ => None,
        };
        if let Some(target) = target {
            *target = Some(dynamic_string(blob, &table, entry)?);
        }
    }
    Ok(strings)
}

fn dynamic_string<Elf>(
    blob: Hash,
    table: &object::read::elf::DynamicTable<'_, Elf>,
    entry: object::read::elf::Dynamic,
) -> Result<Vec<u8>>
where
    Elf: object::read::elf::FileHeader,
{
    table
        .string(entry)
        .map(Vec::from)
        .map_err(|error| invalid(blob, error))
}

fn imports(blob: Hash, file: &object::File<'_>) -> Result<Vec<ElfImport>> {
    let mut result = file
        .imports()
        .map_err(|error| invalid(blob, error))?
        .map(|entry| {
            let entry = entry.map_err(|error| invalid(blob, error))?;
            let name = named(blob, entry.name())?;
            let version = match entry.flags() {
                ImportFlags::Elf { version, .. } => version.map(Vec::from),
                _ => None,
            };
            Ok(ElfImport {
                library: entry.library().to_vec(),
                name,
                version,
                weak: entry.is_weak(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    result.sort();
    Ok(result)
}

fn exports(blob: Hash, file: &object::File<'_>) -> Result<Vec<ElfExport>> {
    let mut result = file
        .exports()
        .map_err(|error| invalid(blob, error))?
        .map(|entry| {
            let entry = entry.map_err(|error| invalid(blob, error))?;
            let name = named(blob, entry.name())?;
            let (binding, version, version_hidden) = match entry.flags() {
                ExportFlags::Elf {
                    st_info,
                    version,
                    version_hidden,
                    ..
                } => (st_info.st_bind().0, version.map(Vec::from), version_hidden),
                _ => (u8::from(entry.is_weak()), None, false),
            };
            Ok(ElfExport {
                name,
                version,
                binding,
                version_hidden,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    result.sort();
    Ok(result)
}

fn named(blob: Hash, name: NameOrOrdinal<&[u8]>) -> Result<Vec<u8>> {
    match name {
        NameOrOrdinal::Name(name) => Ok(name.to_vec()),
        NameOrOrdinal::Ordinal(ordinal) => Err(Error::ElfMetadata {
            hash: blob,
            message: format!("ELF symbol uses unsupported ordinal {ordinal}"),
        }),
    }
}

fn invalid(blob: Hash, error: object::Error) -> Error {
    Error::ElfMetadata {
        hash: blob,
        message: error.to_string(),
    }
}
