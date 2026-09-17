use std::fs::{self, File};
use std::io;
use std::path::Path;

pub(super) fn create_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    // Include existing ancestors: a prior failed startup may have created them
    // without persisting their entries. Resolve symlinks before walking parents.
    for directory in path.canonicalize()?.ancestors() {
        sync_directory(directory)?;
    }
    Ok(())
}

pub(super) fn sync_parent(path: &Path) -> io::Result<()> {
    sync_directory(
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
}

pub(super) fn sync_data(file: &File) -> io::Result<()> {
    #[cfg(test)]
    tests::record(tests::Sync::Data)?;
    file.sync_data()
}

pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    tests::record(tests::Sync::Directory(path.to_owned()))?;
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests;
