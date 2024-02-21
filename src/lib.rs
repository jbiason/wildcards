//! Functions for dealing with wildcards and simple actions over recusive structures.

use std::path::Path;
use std::path::PathBuf;

use regex::Regex;

/// Possible results of the wildcard functions.
#[derive(Debug, PartialEq)]
pub enum WildcardingError {
    /// Whatever was used for this, we can't understand if it is a file or a directory.
    UnknownFormat(PathBuf),

    /// The operation reached an error and couldn't complete.
    OperationFailed(PathBuf, PathBuf),

    /// The source is invalid.
    InvalidSource(PathBuf),

    /// The target is invalid.
    InvalidTarget(PathBuf),

    /// Failed to get the name of the file.
    FilenameFail(PathBuf),

    /// Failed to get the filename as an string.
    InvalidPath(PathBuf),

    /// Missing the parent of a file.
    NoParent(PathBuf),

    /// Can't read the directory.
    ReadError(PathBuf),
}

/// Copy a file (with or without a wildcard) to a target.
pub async fn cp(source: &Path, target: &Path) -> Result<(), WildcardingError> {
    tracing::debug!(?source, ?target);
    // this is the magical closure that says what to do when the operator need to act on a file.
    let closure = |source: &Path, target: &Path| {
        std::fs::copy(source, target).map(|_| ()).map_err(move |_| {
            tracing::debug!(?source, ?target, "copy");
            WildcardingError::OperationFailed(source.to_path_buf(), target.to_path_buf())
        })
    };

    match (source.is_file(), source.is_dir()) {
        (true, true) => Err(WildcardingError::UnknownFormat(source.to_path_buf())),
        (true, false) => do_on_file(source, target, closure).await,
        (false, true) => do_on_dir(source, target, closure).await,
        (false, false) => do_on_mask(source, target, closure).await,
    }
}

/// Move a file (with or without a wildcard) to a target.
pub async fn mv(source: &Path, target: &Path) -> Result<(), WildcardingError> {
    tracing::debug!(?source, ?target);
    let closure = |source: &Path, target: &Path| {
        tracing::debug!(?source, ?target, "rename");
        std::fs::rename(source, target)
            .map(|_| ())
            .map_err(move |_| {
                WildcardingError::OperationFailed(source.to_path_buf(), target.to_path_buf())
            })
    };

    match (source.is_file(), source.is_dir()) {
        (true, true) => Err(WildcardingError::UnknownFormat(source.to_path_buf())),
        (true, false) => do_on_file(source, target, closure).await,
        (false, true) => do_on_dir(source, target, closure).await,
        (false, false) => do_on_mask(source, target, closure).await,
    }
}

/// Remove a file (with or without a wildcard).
pub async fn rm(source: &Path) -> Result<(), WildcardingError> {
    // Quick note: `rm` abuses the functionality below by asking them to transverse the files like
    // cp and mv do, but uses a target that we never touch again.
    tracing::debug!(?source);
    let closure = |source: &Path, _target: &Path| {
        tracing::debug!(?source, "delete");
        std::fs::remove_file(source)
            .map(|_| ())
            .map_err(move |_| WildcardingError::InvalidSource(source.to_path_buf()))
    };

    let target = std::env::temp_dir(); // we will ignore the target, anyway.
    match (source.is_file(), source.is_dir()) {
        (true, true) => Err(WildcardingError::UnknownFormat(source.to_path_buf())),
        (true, false) => do_on_file(source, &target, closure).await,
        (false, true) => do_on_dir(source, &target, closure).await,
        (false, false) => do_on_mask(source, &target, closure).await,
    }
}

/// Act on a file.
async fn do_on_file<T>(source: &Path, target: &Path, op: T) -> Result<(), WildcardingError>
where
    T: Fn(&Path, &Path) -> Result<(), WildcardingError>
        + Send
        + std::marker::Sync
        + std::marker::Copy,
{
    if target.is_dir() {
        let filename = source
            .file_name()
            .ok_or_else(|| WildcardingError::FilenameFail(source.to_path_buf()))?;
        let new_target = target.join(&filename);
        tracing::debug!(?source, ?new_target);
        op(source, &new_target)
    } else {
        op(source, target)
    }
}

/// Act on a directory.
async fn do_on_dir<T>(source: &Path, target: &Path, op: T) -> Result<(), WildcardingError>
where
    T: Fn(&Path, &Path) -> Result<(), WildcardingError>
        + Send
        + std::marker::Sync
        + std::marker::Copy,
{
    if !target.is_dir() {
        Err(WildcardingError::InvalidTarget(target.to_path_buf()))
    } else {
        do_on_mask(&source.join("*"), target, op).await
    }
}

/// Act on files with a certain mask.
async fn do_on_mask<T>(source: &Path, target: &Path, op: T) -> Result<(), WildcardingError>
where
    T: Fn(&Path, &Path) -> Result<(), WildcardingError>
        + Send
        + std::marker::Sync
        + std::marker::Copy,
{
    if let Some(name) = source.file_name() {
        let as_str = name
            .to_str()
            .ok_or_else(|| WildcardingError::InvalidPath(source.to_path_buf()))?;
        if as_str.contains("*") {
            let source = source
                .parent()
                .ok_or_else(|| WildcardingError::NoParent(source.to_path_buf()))?;
            let re = Regex::new(&as_str.replace("*", ".*")).unwrap();

            let mut reader = tokio::fs::read_dir(&source)
                .await
                .map_err(|_| WildcardingError::ReadError(source.to_path_buf()))?;
            while let Ok(Some(entry)) = reader.next_entry().await {
                let entry = entry.path();
                if entry.is_file() {
                    if let Some(name) = entry.file_name() {
                        let as_str = name
                            .to_str()
                            .ok_or_else(|| WildcardingError::InvalidPath(entry.to_path_buf()))?;
                        if re.is_match(as_str) {
                            do_on_file(&source.join(name), target, op).await?;
                        }
                    }
                }
            }
            Ok(())
        } else {
            Err(WildcardingError::InvalidSource(source.to_path_buf()))
        }
    } else {
        Err(WildcardingError::InvalidSource(source.to_path_buf()))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn copy_file_to_file() {
        let temp = std::env::temp_dir();
        let wd = temp.join("cp-file-to-file");
        tokio::fs::create_dir_all(&wd).await.unwrap();

        tokio::fs::write(wd.join("source"), "this is source")
            .await
            .unwrap();

        cp(&wd.join("source"), &wd.join("target")).await.unwrap();

        assert!(wd.join("target").is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn copy_file_to_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("cp-file-to-dir");
        tokio::fs::create_dir_all(&wd).await.unwrap();

        tokio::fs::write(wd.join("source"), "this is source")
            .await
            .unwrap();

        let target = wd.join("target");
        tokio::fs::create_dir_all(&target).await.unwrap();

        cp(&wd.join("source"), &target).await.unwrap();
        assert!(target.join("source").is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn copy_dir_to_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("cp-dir-to-dir");
        let source = wd.join("source");
        let target = wd.join("target");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::create_dir_all(&target).await.unwrap();

        tokio::fs::write(source.join("file1"), "this is file 1")
            .await
            .unwrap();
        tokio::fs::write(source.join("file2"), "this is file 2")
            .await
            .unwrap();

        cp(&source, &target).await.unwrap();

        assert!(target.join("file1").is_file());
        assert!(target.join("file2").is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn copy_mask_to_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("cp-mask-to-dir");
        let source = wd.join("source");
        let target = wd.join("target");

        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::create_dir_all(&target).await.unwrap();

        tokio::fs::write(source.join("file1.txt"), "this is txt")
            .await
            .unwrap();
        tokio::fs::write(source.join("file2.txt"), "this is also txt")
            .await
            .unwrap();
        tokio::fs::write(source.join("file1.glob"), "this is not txt")
            .await
            .unwrap();

        cp(&source.join("*.txt"), &target).await.unwrap();

        assert!(target.join("file1.txt").is_file());
        assert!(target.join("file2.txt").is_file());
        assert!(!target.join("file1.glob").is_file());

        tokio::fs::remove_dir_all(&wd).await.unwrap();
    }

    #[tokio::test]
    async fn copy_star() {
        let temp = std::env::temp_dir();
        let wd = temp.join("cp-mask-star");
        let source = wd.join("source");
        let target = wd.join("target");

        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::create_dir_all(&target).await.unwrap();

        tokio::fs::write(source.join("file1"), "this is 1")
            .await
            .unwrap();
        tokio::fs::write(source.join("file2"), "this is 2")
            .await
            .unwrap();

        cp(&source.join("*"), &target).await.unwrap();

        assert!(target.join("file1").is_file());
        assert!(target.join("file2").is_file());

        tokio::fs::remove_dir_all(&wd).await.unwrap();
    }

    #[tokio::test]
    async fn move_file_to_file() {
        let temp = std::env::temp_dir();
        let wd = temp.join("mv-file-to-file");

        tokio::fs::create_dir_all(&wd).await.unwrap();

        let source = wd.join("source.txt");
        let target = wd.join("target.txt");

        tokio::fs::write(&source, "this is file").await.unwrap();

        mv(&source, &target).await.unwrap();

        assert!(!source.is_file());
        assert!(target.is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn move_file_to_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("mv-file-to-dir");

        let target = wd.join("target");

        tokio::fs::create_dir_all(&target).await.unwrap();

        let source = wd.join("source.txt");

        tokio::fs::write(&source, "this is file").await.unwrap();

        mv(&source, &target).await.unwrap();

        assert!(!source.is_file());
        assert!(target.join("source.txt").is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn move_dir_to_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("mv-dir-to-dir");

        let source = wd.join("source");
        let target = wd.join("target");

        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::create_dir_all(&target).await.unwrap();

        tokio::fs::write(source.join("file1.txt"), "this is one")
            .await
            .unwrap();
        tokio::fs::write(source.join("file2.txt"), "this is two")
            .await
            .unwrap();

        mv(&source, &target).await.unwrap();

        assert!(!source.join("file1.txt").is_file());
        assert!(!source.join("file2.txt").is_file());
        assert!(target.join("file1.txt").is_file());
        assert!(target.join("file2.txt").is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn move_mask_to_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("mv-mask-to-dir");

        let source = wd.join("source");
        let target = wd.join("target");

        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::create_dir_all(&target).await.unwrap();

        tokio::fs::write(source.join("file1.txt"), "this is txt")
            .await
            .unwrap();
        tokio::fs::write(source.join("file2.txt"), "this is txt too")
            .await
            .unwrap();
        tokio::fs::write(source.join("file1.glob"), "this is not txt")
            .await
            .unwrap();

        mv(&source.join("*.txt"), &target).await.unwrap();

        assert!(!source.join("file1.txt").is_file());
        assert!(!source.join("file2.txt").is_file());
        assert!(source.join("file1.glob").is_file());

        assert!(target.join("file1.txt").is_file());
        assert!(target.join("file2.txt").is_file());
        assert!(!target.join("file1.glob").is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn remove_file() {
        let temp = std::env::temp_dir();
        let wd = temp.join("del-file");

        let source = wd.join("source");
        let file = source.join("file.txt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(&file, "this is txt").await.unwrap();

        rm(&file).await.unwrap();

        assert!(!file.is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn remove_dir() {
        let temp = std::env::temp_dir();
        let wd = temp.join("del-dir");
        let source = wd.join("source");
        let file = source.join("file1.txt");

        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(&file, "this is txt").await.unwrap();

        // this is not something that the shell usually support, but we can support due the way we
        // build this thing.
        rm(&source).await.unwrap();

        assert!(!file.is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }

    #[tokio::test]
    async fn remove_mask() {
        let temp = std::env::temp_dir();
        let wd = temp.join("del-mask");
        let source = wd.join("source");
        let file1 = source.join("file1.txt");
        let file2 = source.join("file2.txt");
        let file3 = source.join("file1.glob");

        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(&file1, "this is txt").await.unwrap();
        tokio::fs::write(&file2, "this is also txt").await.unwrap();
        tokio::fs::write(&file3, "this is not txt").await.unwrap();

        rm(&source.join("*.txt")).await.unwrap();

        assert!(!file1.is_file());
        assert!(!file2.is_file());
        assert!(file3.is_file());

        tokio::fs::remove_dir_all(wd).await.unwrap();
    }
}
