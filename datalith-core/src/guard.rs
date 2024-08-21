use std::{fs, path::PathBuf, time::Duration};

use tokio::time;
use uuid::Uuid;

use crate::Datalith;

#[derive(Debug)]
pub(crate) struct PutGuard {
    _datalith: Datalith,
    hash:      [u8; 32],
}

impl Drop for PutGuard {
    #[inline]
    fn drop(&mut self) {
        let mut uploading_files = self._datalith.0._uploading_files.lock().unwrap();

        uploading_files.remove(&self.hash);
    }
}

impl PutGuard {
    pub async fn new(datalith: Datalith, hash: [u8; 32]) -> Self {
        loop {
            {
                let mut uploading_files = datalith.0._uploading_files.lock().unwrap();

                if !uploading_files.contains(&hash) {
                    uploading_files.insert(hash);
                    break;
                }
            }

            time::sleep(Duration::from_millis(10)).await;
        }

        Self {
            _datalith: datalith,
            hash,
        }
    }
}

#[derive(Debug)]
pub(crate) struct OpenGuard {
    _datalith: Datalith,
    id:        Uuid,
}

impl Drop for OpenGuard {
    #[inline]
    fn drop(&mut self) {
        // recover the count

        let mut opening_files = self._datalith.0._opening_files.lock().unwrap();

        let need_remove = {
            let id = opening_files.get_mut(&self.id).unwrap();

            match *id {
                0 | 1 => true,
                _ => {
                    *id -= 1;

                    false
                },
            }
        };

        if need_remove {
            opening_files.remove(&self.id);
        }
    }
}

impl OpenGuard {
    pub async fn new(datalith: Datalith, id: impl Into<Uuid>) -> Self {
        let id = id.into();

        // increase the count
        {
            let mut opening_files = datalith.0._opening_files.lock().unwrap();

            if let Some(opening_count) = opening_files.get_mut(&id) {
                *opening_count += 1;
            } else {
                opening_files.insert(id, 1);
            }
        }

        Self {
            _datalith: datalith,
            id,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DeleteGuard {
    _datalith: Datalith,
    id:        Uuid,
}

impl Drop for DeleteGuard {
    #[inline]
    fn drop(&mut self) {
        let mut _deleting_files = self._datalith.0._deleting_files.lock().unwrap();

        _deleting_files.remove(&self.id);
    }
}

impl DeleteGuard {
    pub async fn new(datalith: Datalith, id: impl Into<Uuid>) -> Self {
        let id = id.into();

        loop {
            {
                let mut deleting_files = datalith.0._deleting_files.lock().unwrap();

                if !deleting_files.contains(&id) {
                    deleting_files.insert(id);
                    break;
                }
            }

            time::sleep(Duration::from_millis(10)).await;
        }

        Self {
            _datalith: datalith,
            id,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TemporaryFileGuard {
    moved:     bool,
    file_path: PathBuf,
}

impl Drop for TemporaryFileGuard {
    #[inline]
    fn drop(&mut self) {
        if !self.moved {
            let _ = fs::remove_file(self.file_path.as_path());
        }
    }
}

impl TemporaryFileGuard {
    #[inline]
    pub fn new(file_path: impl Into<PathBuf>) -> Self {
        let file_path = file_path.into();

        Self {
            moved: false,
            file_path,
        }
    }

    #[inline]
    pub fn set_moved(&mut self) {
        self.moved = true;
    }
}
